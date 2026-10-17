//! The schema as an entity-relationship diagram: table boxes with their columns (primary and
//! foreign keys marked), foreign keys as edges with crow's-foot cardinality, laid out on the
//! server so the diagram reads well with no JavaScript. `static/kernel/dashboard-erd.js` adds
//! pan, zoom, dragging, focus and search on top of the same SVG.
//!
//! Built from `schema::Table` (the catalog the Schema page already reads: SQLite pragmas,
//! Postgres `pg_catalog`), so it works the same on both backends and never reads a row.
//!
//! | Cardinality | When | Parent end | Child end |
//! |-------------|------|------------|-----------|
//! | one to many | the FK columns are NOT NULL | `||` exactly one | `o{` zero or many |
//! | zero-or-one to many | any FK column is nullable | `|o` zero or one | `o{` |
//! | one to one | the FK columns are the primary key or a unique index | `||` or `|o` | `o|` zero or one |
//!
//! Layout (deterministic: the same schema always draws the same picture):
//! 1. tables that take part in a relationship are layered by foreign-key depth (a table with no
//!    parent is depth 0; a child is one more than its deepest parent), parents on the left;
//! 2. each layer is ordered by the barycenter of its neighbours' positions, sweeping right then
//!    left a few times (the classic Sugiyama crossing-reduction pass, without dummy nodes);
//! 3. a layer taller than `MAX_COLUMN` wraps into the next column;
//! 4. tables with no relationship at all go in a grid below, grouped by area.
//!
//! GUIDE: .offrails/guides/models.md
//! GUIDE: .offrails/guides/migrations.md

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

use super::schema::Table;

/// APP: which sample or area a table belongs to, by name prefix (or exact name), for the
/// "colour by area" toggle and the migrations timeline. First match wins; add your own areas
/// (`("billing_", "billing")`) above the samples' rows.
pub const AREAS: &[(&str, &str)] = &[
    ("lab_", "dev-dashboard"),
    ("job", "jobs"),
    ("recurring_executions", "jobs"),
    ("mailbox_", "mailer"),
    ("users", "auth-core"),
    ("sessions", "auth-core"),
    ("user_tokens", "auth-core"),
    ("auth_", "auth-core"),
    ("password_credentials", "auth-password"),
    ("login_links", "auth-magic-link"),
    ("totp_", "auth-totp"),
    ("recovery_codes", "auth-totp"),
    ("trusted_devices", "auth-totp"),
    ("pending_logins", "auth-totp"),
    ("guest", "realtime"),
    ("room", "realtime"),
    ("realtime_", "realtime"),
    ("cache_entries", "host"),
    ("seaql_migrations", "host"),
];

/// The area of a table name (`app` when no row matches).
pub fn area(table: &str) -> &'static str {
    AREAS
        .iter()
        .find(|(prefix, _)| table.starts_with(prefix))
        .map_or("app", |(_, a)| a)
}

/// The table's name prefix (`jobs_queues` → `jobs`), for the "colour by prefix" toggle.
pub fn prefix(table: &str) -> &str {
    table.split('_').next().unwrap_or(table)
}

/// Group colours: `.g0` … `.g9` in `dashboard.css` (light-dark() tokens).
const PALETTE: usize = 10;

// Geometry, in SVG user units (px at zoom 1).
const HEADER: f64 = 30.0;
const ROW: f64 = 20.0;
const PAD_BOTTOM: f64 = 6.0;
const COL_GAP: f64 = 120.0;
const ROW_GAP: f64 = 28.0;
const MARGIN: f64 = 24.0;
/// APP: a layer taller than this wraps into another column (keeps the diagram landscape).
const MAX_COLUMN: f64 = 760.0;
/// APP: the grid of tables without relationships wraps at this width (at least).
const GRID_WIDTH: f64 = 1_800.0;
const CHAR_W: f64 = 7.3;
const TYPE_CHAR_W: f64 = 6.6;
const MIN_W: f64 = 170.0;
const MAX_W: f64 = 380.0;
const TYPE_MAX: usize = 18;
/// Barycenter sweeps (each is one rightward and one leftward pass).
const SWEEPS: usize = 6;

pub struct ErdColumn {
    pub name: String,
    /// Shortened for the box (the detail page has the full type).
    pub ty: String,
    /// The catalog's type, for the Mermaid export.
    pub full_ty: String,
    pub nullable: bool,
    pub pk: bool,
    pub fk: bool,
    pub unique: bool,
    /// Offset of the row's centre from the box's top, with every column and in "keys only".
    pub y: f64,
    pub y_keys: f64,
}

impl ErdColumn {
    pub fn is_key(&self) -> bool {
        self.pk || self.fk || self.unique
    }

    pub fn ys(&self) -> String {
        fmt(self.y)
    }

    pub fn yks(&self) -> String {
        fmt(self.y_keys)
    }

    /// The type as the box shows it (`?` when nullable).
    pub fn ty_text(&self) -> String {
        if self.nullable {
            format!("{}?", self.ty)
        } else {
            self.ty.clone()
        }
    }

    /// "PK", "FK", "PK FK", "UQ", or "".
    pub fn badge(&self) -> String {
        let mut b = Vec::new();
        if self.pk {
            b.push("PK");
        }
        if self.fk {
            b.push("FK");
        }
        if self.unique && !self.pk {
            b.push("UQ");
        }
        b.join(" ")
    }
}

pub struct ErdTable {
    pub name: String,
    pub area: &'static str,
    pub prefix: String,
    /// Palette slot for the area and the prefix (`g0`…`g7`).
    pub area_class: String,
    pub prefix_class: String,
    pub rows: Option<i64>,
    pub columns: Vec<ErdColumn>,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub h_keys: f64,
    /// Takes part in no relationship (drawn in the grid below).
    pub isolated: bool,
}

impl ErdTable {
    /// The accessible name of the box ("lab_orders: 6 columns, 1 foreign key, 120 rows").
    pub fn label(&self) -> String {
        let fks = self.columns.iter().filter(|c| c.fk).count();
        let mut s = format!(
            "{}: {} column{}",
            self.name,
            self.columns.len(),
            if self.columns.len() == 1 { "" } else { "s" }
        );
        if fks > 0 {
            let _ = write!(
                s,
                ", {fks} foreign key column{}",
                if fks == 1 { "" } else { "s" }
            );
        }
        if let Some(n) = self.rows {
            let _ = write!(s, ", {n} row{}", if n == 1 { "" } else { "s" });
        }
        s
    }

    pub fn tx(&self) -> String {
        format!("translate({} {})", fmt(self.x), fmt(self.y))
    }

    pub fn ws(&self) -> String {
        fmt(self.w)
    }

    pub fn hs(&self) -> String {
        fmt(self.h)
    }

    pub fn hks(&self) -> String {
        fmt(self.h_keys)
    }

    /// The x of right-aligned text (row counts, types).
    pub fn right(&self) -> String {
        fmt(self.w - 10.0)
    }

    /// The coloured header band: the box's top with rounded corners.
    pub fn head_path(&self) -> String {
        format!(
            "M0 6a6 6 0 0 1 6-6h{}a6 6 0 0 1 6 6v{}h-{}z",
            fmt(self.w - 12.0),
            fmt(HEADER - 6.0),
            fmt(self.w)
        )
    }

    /// The header's bottom rule.
    pub fn rule(&self) -> String {
        format!("M0 {}h{}", fmt(HEADER), fmt(self.w))
    }
}

/// End markers: what one side of a relationship can be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum End {
    One,
    ZeroOrOne,
    ZeroOrMany,
}

impl End {
    pub fn key(self) -> &'static str {
        match self {
            End::One => "one",
            End::ZeroOrOne => "zero-one",
            End::ZeroOrMany => "many",
        }
    }

    /// Mermaid's half of the relationship arrow, read on the side of `left`.
    fn mermaid(self, left: bool) -> &'static str {
        match (self, left) {
            (End::One, _) => "||",
            (End::ZeroOrOne, true) => "|o",
            (End::ZeroOrOne, false) => "o|",
            (End::ZeroOrMany, true) => "}o",
            (End::ZeroOrMany, false) => "o{",
        }
    }

    fn words(self) -> &'static str {
        match self {
            End::One => "exactly one",
            End::ZeroOrOne => "zero or one",
            End::ZeroOrMany => "zero or many",
        }
    }
}

pub struct ErdEdge {
    /// The table holding the foreign key.
    pub child: String,
    pub parent: String,
    pub columns: Vec<String>,
    pub to: Vec<String>,
    pub on_delete: String,
    pub parent_end: End,
    pub child_end: End,
    /// Indexes into `Erd::tables` and into each table's columns (for the script).
    pub child_idx: usize,
    pub parent_idx: usize,
    pub child_col: usize,
    pub parent_col: usize,
    /// The SVG path of the line and of both end glyphs (with every column shown).
    pub d: String,
    pub glyphs: String,
}

impl ErdEdge {
    /// "Each lab_order_items row belongs to exactly one lab_orders row; a lab_orders row has zero or many lab_order_items."
    pub fn sentence(&self) -> String {
        format!(
            "{} ({}) → {} ({}): each {} row points at {} {} row; a {} row has {} {} rows. On delete: {}.",
            self.child,
            self.columns.join(", "),
            self.parent,
            self.to.join(", "),
            self.child,
            self.parent_end.words(),
            self.parent,
            self.parent,
            self.child_end.words(),
            self.child,
            self.on_delete
        )
    }

    pub fn self_ref(&self) -> bool {
        self.child == self.parent
    }
}

pub struct Legend {
    pub key: String,
    pub class: String,
}

pub struct Erd {
    pub tables: Vec<ErdTable>,
    pub edges: Vec<ErdEdge>,
    pub width: f64,
    pub height: f64,
    pub areas: Vec<Legend>,
    pub prefixes: Vec<Legend>,
}

impl Erd {
    pub fn view_box(&self) -> String {
        format!("0 0 {} {}", fmt(self.width), fmt(self.height))
    }

    pub fn w(&self) -> String {
        fmt(self.width)
    }

    pub fn h(&self) -> String {
        fmt(self.height)
    }

    /// The table a name refers to.
    pub fn find(&self, name: &str) -> Option<&ErdTable> {
        self.tables.iter().find(|t| t.name == name)
    }

    /// Edges touching `table` (either end).
    pub fn edges_of<'a>(&'a self, table: &'a str) -> impl Iterator<Item = &'a ErdEdge> + 'a {
        self.edges
            .iter()
            .filter(move |e| e.child == table || e.parent == table)
    }
}

/// Two decimals at most, no trailing zeros: stable, compact SVG.
pub fn fmt(v: f64) -> String {
    let s = format!("{v:.1}");
    s.strip_suffix(".0").map(str::to_owned).unwrap_or(s)
}

fn short_type(ty: &str) -> String {
    let t = ty
        .replace("timestamp_with_timezone_text", "timestamptz")
        .replace("json_text", "json")
        .replace("timestamp with time zone", "timestamptz")
        .replace("timestamp without time zone", "timestamp")
        .replace("character varying", "varchar")
        .replace("double precision", "double");
    if t.chars().count() > TYPE_MAX {
        let cut: String = t.chars().take(TYPE_MAX - 1).collect();
        format!("{cut}…")
    } else {
        t
    }
}

/// Builds the diagram from the catalog: cardinalities, then the layout.
pub fn build(tables: &[Table]) -> Erd {
    let names: HashMap<&str, usize> = tables
        .iter()
        .enumerate()
        .map(|(i, t)| (t.name.as_str(), i))
        .collect();
    // Palette slots in first-seen order of the sorted names: stable across reloads.
    let mut area_slots: BTreeMap<&str, usize> = BTreeMap::new();
    let mut prefix_slots: BTreeMap<String, usize> = BTreeMap::new();
    for t in tables {
        let n = area_slots.len();
        area_slots.entry(area(&t.name)).or_insert(n % PALETTE);
        let n = prefix_slots.len();
        prefix_slots
            .entry(prefix(&t.name).to_owned())
            .or_insert(n % PALETTE);
    }
    let mut out: Vec<ErdTable> = tables
        .iter()
        .map(|t| {
            let unique_sets: Vec<Vec<&str>> = t
                .indexes
                .iter()
                .filter(|i| i.unique)
                .map(|i| i.columns.iter().map(String::as_str).collect())
                .collect();
            let fk_cols: HashSet<&str> = t
                .fks
                .iter()
                .flat_map(|f| f.columns.iter().map(String::as_str))
                .collect();
            let mut columns: Vec<ErdColumn> = t
                .columns
                .iter()
                .map(|c| ErdColumn {
                    name: c.name.clone(),
                    ty: short_type(&c.ty),
                    full_ty: c.ty.clone(),
                    nullable: c.nullable,
                    pk: c.pk,
                    fk: fk_cols.contains(c.name.as_str()),
                    unique: unique_sets.iter().any(|s| s.len() == 1 && s[0] == c.name),
                    y: 0.0,
                    y_keys: 0.0,
                })
                .collect();
            let mut k = 0.0;
            for (i, c) in columns.iter_mut().enumerate() {
                c.y = HEADER + ROW * (i as f64 + 0.5);
                if c.is_key() {
                    c.y_keys = HEADER + ROW * (k + 0.5);
                    k += 1.0;
                }
            }
            // A column hidden in "keys only" attaches its edges to the header.
            for c in columns.iter_mut().filter(|c| !c.is_key()) {
                c.y_keys = HEADER / 2.0;
            }
            let widest_name = columns
                .iter()
                .map(|c| c.name.chars().count())
                .max()
                .unwrap_or(0);
            let widest_type = columns
                .iter()
                .map(|c| c.ty.chars().count())
                .max()
                .unwrap_or(0);
            let w = (14.0
                + 34.0
                + widest_name as f64 * CHAR_W
                + 18.0
                + widest_type as f64 * TYPE_CHAR_W
                + 12.0)
                .max(t.name.chars().count() as f64 * (CHAR_W + 0.7) + 70.0)
                .clamp(MIN_W, MAX_W)
                .round();
            let h = HEADER + ROW * columns.len() as f64 + PAD_BOTTOM;
            let h_keys = HEADER + ROW * k + PAD_BOTTOM;
            let a = area(&t.name);
            let p = prefix(&t.name).to_owned();
            ErdTable {
                area_class: format!("g{}", area_slots.get(a).copied().unwrap_or(0)),
                prefix_class: format!("g{}", prefix_slots.get(&p).copied().unwrap_or(0)),
                name: t.name.clone(),
                area: a,
                prefix: p,
                rows: t.rows,
                columns,
                x: 0.0,
                y: 0.0,
                w,
                h,
                h_keys,
                isolated: true,
            }
        })
        .collect();

    let mut edges = Vec::new();
    for (ci, t) in tables.iter().enumerate() {
        for fk in &t.fks {
            let Some(&pi) = names.get(fk.table.as_str()) else {
                continue;
            };
            let parent = &tables[pi];
            let nullable = fk.columns.iter().any(|c| {
                t.columns
                    .iter()
                    .find(|x| &x.name == c)
                    .is_some_and(|x| x.nullable)
            });
            let pk: Vec<&str> = t
                .columns
                .iter()
                .filter(|c| c.pk)
                .map(|c| c.name.as_str())
                .collect();
            let same = |set: &[&str]| {
                set.len() == fk.columns.len()
                    && fk.columns.iter().all(|c| set.contains(&c.as_str()))
            };
            let unique = same(&pk)
                || t.indexes
                    .iter()
                    .filter(|i| i.unique)
                    .any(|i| same(&i.columns.iter().map(String::as_str).collect::<Vec<_>>()));
            let col_index = |tb: &Table, name: Option<&String>| {
                name.and_then(|n| tb.columns.iter().position(|c| &c.name == n))
                    .unwrap_or(0)
            };
            edges.push(ErdEdge {
                child: t.name.clone(),
                parent: parent.name.clone(),
                columns: fk.columns.clone(),
                to: fk.to.clone(),
                on_delete: fk.on_delete.clone(),
                parent_end: if nullable { End::ZeroOrOne } else { End::One },
                child_end: if unique {
                    End::ZeroOrOne
                } else {
                    End::ZeroOrMany
                },
                child_idx: ci,
                parent_idx: pi,
                child_col: col_index(t, fk.columns.first()),
                parent_col: col_index(parent, fk.to.first()),
                d: String::new(),
                glyphs: String::new(),
            });
            if ci != pi {
                out[ci].isolated = false;
                out[pi].isolated = false;
            }
        }
    }

    let (width, height) = layout(&mut out, &edges);
    for e in &mut edges {
        let (d, glyphs) = route(
            &out[e.parent_idx],
            out[e.parent_idx]
                .columns
                .get(e.parent_col)
                .map_or(HEADER / 2.0, |c| c.y),
            &out[e.child_idx],
            out[e.child_idx]
                .columns
                .get(e.child_col)
                .map_or(HEADER / 2.0, |c| c.y),
            e.parent_end,
            e.child_end,
        );
        e.d = d;
        e.glyphs = glyphs;
    }
    let legend = |m: BTreeMap<String, usize>| {
        m.into_iter()
            .map(|(key, slot)| Legend {
                key,
                class: format!("g{slot}"),
            })
            .collect()
    };
    Erd {
        areas: legend(
            area_slots
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
        ),
        prefixes: legend(prefix_slots),
        tables: out,
        edges,
        width,
        height,
    }
}

/// Positions every table; returns the diagram's size.
fn layout(tables: &mut [ErdTable], edges: &[ErdEdge]) -> (f64, f64) {
    let n = tables.len();
    let mut parents: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for e in edges.iter().filter(|e| e.child_idx != e.parent_idx) {
        parents[e.child_idx].push(e.parent_idx);
        children[e.parent_idx].push(e.child_idx);
    }
    // 1. Depth by longest path from a root; a cycle (two tables pointing at each other) is cut
    // where the walk meets it.
    let mut depth: Vec<Option<usize>> = vec![None; n];
    fn walk(
        i: usize,
        parents: &[Vec<usize>],
        depth: &mut [Option<usize>],
        on_path: &mut Vec<bool>,
    ) -> usize {
        if let Some(d) = depth[i] {
            return d;
        }
        on_path[i] = true;
        let mut d = 0;
        for &p in &parents[i] {
            if !on_path[p] {
                d = d.max(walk(p, parents, depth, on_path) + 1);
            }
        }
        on_path[i] = false;
        depth[i] = Some(d);
        d
    }
    let mut on_path = vec![false; n];
    let connected: Vec<usize> = (0..n).filter(|&i| !tables[i].isolated).collect();
    for &i in &connected {
        walk(i, &parents, &mut depth, &mut on_path);
    }
    let layers_n = connected
        .iter()
        .filter_map(|&i| depth[i])
        .max()
        .map_or(0, |d| d + 1);
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); layers_n];
    let mut by_name = connected.clone();
    by_name.sort_by(|&a, &b| {
        (tables[a].area, &tables[a].name).cmp(&(tables[b].area, &tables[b].name))
    });
    for &i in &by_name {
        if let Some(d) = depth[i] {
            layers[d].push(i);
        }
    }
    // 2. Barycenter sweeps: a table moves to the average position of its neighbours in the
    // layers already placed.
    let position = |layers: &[Vec<usize>]| {
        let mut pos = vec![0.0; n];
        for layer in layers {
            for (k, &i) in layer.iter().enumerate() {
                pos[i] = (k as f64 + 0.5) / layer.len().max(1) as f64;
            }
        }
        pos
    };
    let order = |layer: &mut Vec<usize>, neighbours: &[Vec<usize>], pos: &[f64]| {
        let mut keyed: Vec<(f64, usize, usize)> = layer
            .iter()
            .enumerate()
            .map(|(k, &i)| {
                let own = (k as f64 + 0.5) / layer.len().max(1) as f64;
                let ns = &neighbours[i];
                let bary = if ns.is_empty() {
                    own
                } else {
                    ns.iter().map(|&j| pos[j]).sum::<f64>() / ns.len() as f64
                };
                (bary, k, i)
            })
            .collect();
        keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        *layer = keyed.into_iter().map(|(_, _, i)| i).collect();
    };
    for _ in 0..SWEEPS {
        for l in 1..layers.len() {
            let pos = position(&layers);
            order(&mut layers[l], &parents, &pos);
        }
        for l in (0..layers.len().saturating_sub(1)).rev() {
            let pos = position(&layers);
            order(&mut layers[l], &children, &pos);
        }
    }
    // 3. Columns: a layer wraps when it grows past MAX_COLUMN.
    let mut columns: Vec<Vec<usize>> = Vec::new();
    for layer in &layers {
        let mut col: Vec<usize> = Vec::new();
        let mut h = 0.0;
        for &i in layer {
            let th = tables[i].h + ROW_GAP;
            if !col.is_empty() && h + th > MAX_COLUMN {
                columns.push(std::mem::take(&mut col));
                h = 0.0;
            }
            col.push(i);
            h += th;
        }
        if !col.is_empty() {
            columns.push(col);
        }
    }
    let heights: Vec<f64> = columns
        .iter()
        .map(|c| {
            c.iter().map(|&i| tables[i].h).sum::<f64>() + ROW_GAP * c.len().saturating_sub(1) as f64
        })
        .collect();
    let tallest = heights.iter().copied().fold(0.0, f64::max);
    let mut x = MARGIN;
    for (c, h) in columns.iter().zip(&heights) {
        let w = c.iter().map(|&i| tables[i].w).fold(0.0, f64::max);
        // Centred vertically: short layers sit beside the middle of tall ones.
        let mut y = MARGIN + ((tallest - h) / 2.0).round();
        for &i in c {
            tables[i].x = x + ((w - tables[i].w) / 2.0).round();
            tables[i].y = y;
            y += tables[i].h + ROW_GAP;
        }
        x += w + COL_GAP;
    }
    let mut width = if columns.is_empty() {
        MARGIN
    } else {
        x - COL_GAP + MARGIN
    };
    let mut height = if columns.is_empty() {
        MARGIN
    } else {
        MARGIN + tallest + MARGIN
    };
    // 4. The grid: isolated tables in rows, grouped by area then name.
    let mut isolated: Vec<usize> = (0..n).filter(|&i| tables[i].isolated).collect();
    isolated.sort_by(|&a, &b| {
        (tables[a].area, &tables[a].name).cmp(&(tables[b].area, &tables[b].name))
    });
    if !isolated.is_empty() {
        let wrap = width.max(GRID_WIDTH);
        let (mut gx, mut gy, mut row_h) = (MARGIN, height + 12.0, 0.0_f64);
        for &i in &isolated {
            if gx > MARGIN && gx + tables[i].w > wrap - MARGIN {
                gx = MARGIN;
                gy += row_h + ROW_GAP;
                row_h = 0.0;
            }
            tables[i].x = gx;
            tables[i].y = gy;
            gx += tables[i].w + 40.0;
            row_h = row_h.max(tables[i].h);
            width = width.max(gx - 40.0 + MARGIN);
        }
        height = gy + row_h + MARGIN;
    }
    (width.ceil(), height.ceil())
}

/// The path from the parent's key row to the child's foreign-key row, and both end glyphs.
/// `dashboard-erd.js` has the same function (`route`) for when a table is dragged: keep them in
/// step.
pub fn route(
    parent: &ErdTable,
    py: f64,
    child: &ErdTable,
    cy: f64,
    parent_end: End,
    child_end: End,
) -> (String, String) {
    let (y1, y2) = (parent.y + py, child.y + cy);
    if std::ptr::eq(parent, child) {
        // Self-reference: a loop off the right side.
        let x = parent.x + parent.w;
        let d = format!(
            "M{} {}C{} {} {} {} {} {}",
            fmt(x),
            fmt(y2),
            fmt(x + 56.0),
            fmt(y2),
            fmt(x + 56.0),
            fmt(y1),
            fmt(x),
            fmt(y1)
        );
        let g = glyph(x, y1, 1.0, parent_end) + &glyph(x, y2, 1.0, child_end);
        return (d, g);
    }
    let pc = parent.x + parent.w / 2.0;
    let cc = child.x + child.w / 2.0;
    let (x1, s1, x2, s2) = if (pc - cc).abs() < 1.0 {
        // Same column: both ends on the right, bowing out.
        (parent.x + parent.w, 1.0, child.x + child.w, 1.0)
    } else if cc > pc {
        (parent.x + parent.w, 1.0, child.x, -1.0)
    } else {
        (parent.x, -1.0, child.x + child.w, 1.0)
    };
    let bend = if s1 == s2 {
        70.0
    } else {
        ((x2 - x1).abs() / 2.0).max(40.0)
    };
    let d = format!(
        "M{} {}C{} {} {} {} {} {}",
        fmt(x1),
        fmt(y1),
        fmt(x1 + s1 * bend),
        fmt(y1),
        fmt(x2 + s2 * bend),
        fmt(y2),
        fmt(x2),
        fmt(y2)
    );
    let g = glyph(x1, y1, s1, parent_end) + &glyph(x2, y2, s2, child_end);
    (d, g)
}

/// A crow's-foot end at (x, y), the line leaving in direction `s` (+1 right, -1 left).
fn glyph(x: f64, y: f64, s: f64, end: End) -> String {
    let p = |dx: f64| fmt(x + s * dx);
    // A full circle of radius 4 centred `dx` from the table, on the line.
    let ring = |dx: f64| {
        format!(
            "M{} {}a4 4 0 1 0 8 0a4 4 0 1 0 -8 0",
            fmt(x + s * dx - 4.0),
            fmt(y)
        )
    };
    let tick = |dx: f64| format!("M{} {}v12", p(dx), fmt(y - 6.0));
    match end {
        End::One => tick(8.0) + &tick(13.0),
        End::ZeroOrOne => tick(8.0) + &ring(17.0),
        End::ZeroOrMany => {
            format!(
                "M{x0} {}L{tip} {y0}M{x0} {y0}L{tip} {y0}M{x0} {}L{tip} {y0}",
                fmt(y - 6.0),
                fmt(y + 6.0),
                x0 = fmt(x),
                tip = p(11.0),
                y0 = fmt(y)
            ) + &ring(17.0)
        }
    }
}

/// Mermaid `erDiagram` text: paste it into docs, a pull request or an LLM.
pub fn mermaid(erd: &Erd) -> String {
    let mut s = String::from("erDiagram\n");
    for t in &erd.tables {
        let _ = writeln!(s, "    {} {{", t.name);
        for c in &t.columns {
            let ty: String = c
                .full_ty
                .chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
                .collect();
            let ty = if ty.is_empty() { "unknown".into() } else { ty };
            let mut keys = Vec::new();
            if c.pk {
                keys.push("PK");
            }
            if c.fk {
                keys.push("FK");
            }
            if c.unique && !c.pk {
                keys.push("UK");
            }
            let _ = write!(s, "        {ty} {}", c.name);
            if !keys.is_empty() {
                let _ = write!(s, " {}", keys.join(", "));
            }
            if c.nullable {
                s.push_str(" \"nullable\"");
            }
            s.push('\n');
        }
        s.push_str("    }\n");
    }
    for e in &erd.edges {
        let _ = writeln!(
            s,
            "    {} {}--{} {} : \"{}\"",
            e.parent,
            e.parent_end.mermaid(true),
            e.child_end.mermaid(false),
            e.child,
            e.columns.join(", ")
        );
    }
    s
}

/// The ERD as a standalone SVG file (Export SVG): fixed light colours as presentation
/// attributes, since the file is opened without the dashboard's stylesheet.
pub fn standalone_svg(erd: &Erd) -> String {
    // APP: the export's colours.
    const INK: &str = "#1c1917";
    const MUTED: &str = "#57534e";
    const LINE: &str = "#78716c";
    const PAPER: &str = "#ffffff";
    const HEAD: [&str; PALETTE] = [
        "#e0e7ff", "#ccfbf1", "#fef3c7", "#ffe4e6", "#e0f2fe", "#ede9fe", "#dcfce7", "#f5f5f4",
        "#fae8ff", "#ffedd5",
    ];
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let mut s = String::new();
    let _ = write!(
        s,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\" \
         font-family=\"ui-monospace, SFMono-Regular, Menlo, monospace\" font-size=\"12\">\
         <title>Schema</title><rect width=\"{w}\" height=\"{h}\" fill=\"{PAPER}\"/>",
        w = fmt(erd.width),
        h = fmt(erd.height)
    );
    for e in &erd.edges {
        let _ = write!(
            s,
            "<path d=\"{}\" fill=\"none\" stroke=\"{LINE}\" stroke-width=\"1.4\"/>\
             <path d=\"{}\" fill=\"{PAPER}\" stroke=\"{LINE}\" stroke-width=\"1.4\"/>",
            e.d, e.glyphs
        );
    }
    for t in &erd.tables {
        let slot = t
            .area_class
            .trim_start_matches('g')
            .parse::<usize>()
            .unwrap_or(0)
            % PALETTE;
        let _ = write!(
            s,
            "<g transform=\"{}\"><rect width=\"{}\" height=\"{}\" rx=\"6\" fill=\"{PAPER}\" stroke=\"{LINE}\"/>\
             <path d=\"M0 6a6 6 0 0 1 6-6h{}a6 6 0 0 1 6 6v{}h-{}z\" fill=\"{}\"/>\
             <text x=\"10\" y=\"19\" font-weight=\"700\" fill=\"{INK}\">{}</text>",
            t.tx(),
            fmt(t.w),
            fmt(t.h),
            fmt(t.w - 12.0),
            fmt(HEADER - 6.0),
            fmt(t.w),
            HEAD[slot],
            esc(&t.name)
        );
        for c in &t.columns {
            let _ = write!(
                s,
                "<text x=\"10\" y=\"{y}\" font-size=\"10\" fill=\"{MUTED}\">{}</text>\
                 <text x=\"44\" y=\"{y}\" fill=\"{INK}\">{}</text>\
                 <text x=\"{}\" y=\"{y}\" text-anchor=\"end\" fill=\"{MUTED}\">{}{}</text>",
                c.badge(),
                esc(&c.name),
                fmt(t.w - 10.0),
                esc(&c.ty),
                if c.nullable { "?" } else { "" },
                y = fmt(c.y + 4.0)
            );
        }
        s.push_str("</g>");
    }
    s.push_str("</svg>\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::dashboard::schema::{Column, ForeignKey, Index};

    fn col(name: &str, nullable: bool, pk: bool) -> Column {
        Column {
            name: name.into(),
            ty: "bigint".into(),
            nullable,
            pk,
        }
    }

    fn table(name: &str, cols: Vec<Column>, fks: Vec<(&str, &str)>, unique: &[&str]) -> Table {
        Table {
            name: name.into(),
            rows: Some(0),
            bytes: None,
            columns: cols,
            indexes: unique
                .iter()
                .map(|c| Index {
                    name: format!("{name}_{c}_key"),
                    columns: vec![(*c).into()],
                    unique: true,
                })
                .collect(),
            fks: fks
                .into_iter()
                .map(|(c, t)| ForeignKey {
                    columns: vec![c.into()],
                    table: t.into(),
                    to: vec!["id".into()],
                    on_delete: "cascade".into(),
                })
                .collect(),
        }
    }

    fn sample() -> Vec<Table> {
        vec![
            table("cache_entries", vec![col("key", false, true)], vec![], &[]),
            table(
                "lab_order_items",
                vec![col("id", false, true), col("order_id", false, false)],
                vec![("order_id", "lab_orders")],
                &[],
            ),
            table("lab_orders", vec![col("id", false, true)], vec![], &[]),
            table(
                "profiles",
                vec![col("id", false, true), col("user_id", true, false)],
                vec![("user_id", "users")],
                &["user_id"],
            ),
            table(
                "users",
                vec![col("id", false, true), col("invited_by", true, false)],
                vec![("invited_by", "users")],
                &[],
            ),
        ]
    }

    #[test]
    fn cardinality_follows_nullability_and_uniqueness() {
        let erd = build(&sample());
        let items = erd
            .edges
            .iter()
            .find(|e| e.child == "lab_order_items")
            .unwrap();
        assert_eq!(
            (items.parent_end, items.child_end),
            (End::One, End::ZeroOrMany)
        );
        let profile = erd.edges.iter().find(|e| e.child == "profiles").unwrap();
        assert_eq!(
            (profile.parent_end, profile.child_end),
            (End::ZeroOrOne, End::ZeroOrOne),
            "a nullable unique foreign key is zero-or-one to zero-or-one"
        );
        assert!(erd.edges.iter().any(|e| e.self_ref()));
        let m = mermaid(&erd);
        assert!(m.starts_with("erDiagram\n"));
        assert!(m.contains("lab_orders ||--o{ lab_order_items : \"order_id\""));
        assert!(m.contains("users |o--o| profiles : \"user_id\""));
        assert!(m.contains("bigint id PK"));
    }

    #[test]
    fn the_layout_is_deterministic_layered_and_overlap_free() {
        let a = build(&sample());
        let b = build(&sample());
        let pos = |e: &Erd| {
            e.tables
                .iter()
                .map(|t| (t.name.clone(), t.x, t.y))
                .collect::<Vec<_>>()
        };
        assert_eq!(pos(&a), pos(&b));
        let t = |n: &str| a.find(n).unwrap();
        assert!(
            t("lab_orders").x < t("lab_order_items").x,
            "parents left of children"
        );
        assert!(t("users").x < t("profiles").x);
        assert!(t("cache_entries").isolated);
        assert!(
            t("cache_entries").y > t("lab_orders").y,
            "isolated tables go below"
        );
        for (i, p) in a.tables.iter().enumerate() {
            assert!(p.x >= 0.0 && p.y >= 0.0 && p.x + p.w <= a.width && p.y + p.h <= a.height);
            for q in &a.tables[i + 1..] {
                let apart =
                    p.x + p.w <= q.x || q.x + q.w <= p.x || p.y + p.h <= q.y || q.y + q.h <= p.y;
                assert!(apart, "{} overlaps {}", p.name, q.name);
            }
        }
        let svg = standalone_svg(&a);
        assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
        assert!(!svg.contains("class="), "the export styles itself");
    }

    #[test]
    fn barycenter_ordering_untangles_crossed_children() {
        // Two parents, two children, each child of one parent: the children end up in the
        // parents' order, so the edges don't cross.
        let tables = vec![
            table("a_parent", vec![col("id", false, true)], vec![], &[]),
            table("b_parent", vec![col("id", false, true)], vec![], &[]),
            table(
                "a_child",
                vec![col("id", false, true), col("p", false, false)],
                vec![("p", "b_parent")],
                &[],
            ),
            table(
                "b_child",
                vec![col("id", false, true), col("p", false, false)],
                vec![("p", "a_parent")],
                &[],
            ),
        ];
        let erd = build(&tables);
        let y = |n: &str| erd.find(n).unwrap().y;
        assert!(y("a_parent") < y("b_parent"));
        assert!(
            y("b_child") < y("a_child"),
            "child of the upper parent goes up"
        );
    }

    #[test]
    fn markup_in_names_is_escaped_in_the_export() {
        let mut t = sample();
        t[0].columns[0].name = "<script>".into();
        let svg = standalone_svg(&build(&t));
        assert!(!svg.contains("<script>"));
        assert!(svg.contains("&lt;script&gt;"));
    }
}
