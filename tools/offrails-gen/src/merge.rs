//! Line-based three-way merge (diff3): the base a part was installed from, the app's copy, and
//! the part's new version. Changes on one side apply cleanly; the same change on both sides is
//! taken once; different changes to the same lines are a conflict, written with markers.

/// One side's change to the base: lines `[start, end)` replaced by `lines`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Change {
    start: usize,
    end: usize,
    lines: Vec<String>,
}

/// The changes that turn `a` into `b` (longest common subsequence of lines).
fn changes(a: &[String], b: &[String]) -> Vec<Change> {
    let (n, m) = (a.len(), b.len());
    // lcs[i][j]: the LCS length of a[i..] and b[j..].
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    let mut cur: Option<Change> = None;
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            if let Some(c) = cur.take() {
                out.push(c);
            }
            i += 1;
            j += 1;
        } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            cur.get_or_insert(Change {
                start: i,
                end: i,
                lines: Vec::new(),
            })
            .lines
            .push(b[j].clone());
            j += 1;
        } else {
            let c = cur.get_or_insert(Change {
                start: i,
                end: i,
                lines: Vec::new(),
            });
            c.end = i + 1;
            i += 1;
        }
    }
    if let Some(c) = cur.take() {
        out.push(c);
    }
    out
}

/// `base[start..end]` with `changes` (all inside it) applied.
fn apply(base: &[String], start: usize, end: usize, changes: &[&Change]) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = start;
    for c in changes {
        out.extend_from_slice(&base[at..c.start]);
        out.extend(c.lines.iter().cloned());
        at = c.end;
    }
    out.extend_from_slice(&base[at..end]);
    out
}

pub struct Merged {
    pub text: String,
    pub conflicts: usize,
}

fn lines(s: &str) -> Vec<String> {
    s.replace("\r\n", "\n")
        .split_inclusive('\n')
        .map(str::to_owned)
        .collect()
}

/// Merges `ours` (the app) and `theirs` (the new part) against `base` (the installed part).
/// `labels`: (ours, base, theirs) for the conflict markers.
pub fn merge3(base: &str, ours: &str, theirs: &str, labels: (&str, &str, &str)) -> Merged {
    let (b, o, t) = (lines(base), lines(ours), lines(theirs));
    let co = changes(&b, &o);
    let ct = changes(&b, &t);
    let mut out: Vec<String> = Vec::new();
    let mut conflicts = 0;
    let (mut io, mut it) = (0, 0);
    let mut at = 0;
    while io < co.len() || it < ct.len() {
        let next_o = co.get(io).map(|c| c.start);
        let next_t = ct.get(it).map(|c| c.start);
        let start = match (next_o, next_t) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => break,
        };
        out.extend_from_slice(&b[at..start]);
        let mut end = start;
        let (go, gt) = (io, it);
        // A group: every change that overlaps it, or starts at its start (two insertions at
        // one spot interact).
        loop {
            let mut grew = false;
            while io < co.len() && (co[io].start < end || co[io].start == start) {
                end = end.max(co[io].end);
                io += 1;
                grew = true;
            }
            while it < ct.len() && (ct[it].start < end || ct[it].start == start) {
                end = end.max(ct[it].end);
                it += 1;
                grew = true;
            }
            if !grew {
                break;
            }
        }
        let ours_g: Vec<&Change> = co[go..io].iter().collect();
        let theirs_g: Vec<&Change> = ct[gt..it].iter().collect();
        let mine = apply(&b, start, end, &ours_g);
        let new = apply(&b, start, end, &theirs_g);
        if theirs_g.is_empty() || mine == new {
            out.extend(mine);
        } else if ours_g.is_empty() {
            out.extend(new);
        } else {
            conflicts += 1;
            let nl = |v: &mut Vec<String>| {
                if let Some(l) = v.last_mut()
                    && !l.ends_with('\n')
                {
                    l.push('\n');
                }
            };
            let (mut m2, mut b2, mut n2) = (mine, b[start..end].to_vec(), new);
            nl(&mut m2);
            nl(&mut b2);
            nl(&mut n2);
            out.push(format!("<<<<<<< {}\n", labels.0));
            out.extend(m2);
            out.push(format!("||||||| {}\n", labels.1));
            out.extend(b2);
            out.push("=======\n".to_owned());
            out.extend(n2);
            out.push(format!(">>>>>>> {}\n", labels.2));
        }
        at = end;
    }
    out.extend_from_slice(&b[at..]);
    Merged {
        text: out.concat(),
        conflicts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const L: (&str, &str, &str) = ("app", "part 1", "part 2");

    #[test]
    fn changes_on_different_lines_both_apply() {
        let base = "a\nb\nc\nd\ne\n";
        let ours = "a\nB (app)\nc\nd\ne\n";
        let theirs = "a\nb\nc\nd\nE (part)\nf\n";
        let m = merge3(base, ours, theirs, L);
        assert_eq!(m.conflicts, 0);
        assert_eq!(m.text, "a\nB (app)\nc\nd\nE (part)\nf\n");
    }

    #[test]
    fn the_same_change_on_both_sides_is_taken_once() {
        let m = merge3("a\nb\n", "a\nx\n", "a\nx\n", L);
        assert_eq!((m.conflicts, m.text.as_str()), (0, "a\nx\n"));
    }

    #[test]
    fn different_changes_to_the_same_line_conflict_with_markers() {
        let m = merge3("a\nb\nc\n", "a\nmine\nc\n", "a\ntheirs\nc\n", L);
        assert_eq!(m.conflicts, 1);
        assert_eq!(
            m.text,
            "a\n<<<<<<< app\nmine\n||||||| part 1\nb\n=======\ntheirs\n>>>>>>> part 2\nc\n"
        );
    }

    #[test]
    fn untouched_by_the_app_takes_the_new_part() {
        let m = merge3("a\nb\n", "a\nb\n", "a\nb\nc\n", L);
        assert_eq!((m.conflicts, m.text.as_str()), (0, "a\nb\nc\n"));
        let m = merge3("a\nb\n", "z\na\nb\n", "a\nb\n", L);
        assert_eq!((m.conflicts, m.text.as_str()), (0, "z\na\nb\n"));
    }

    #[test]
    fn insertions_at_the_same_spot_conflict() {
        let m = merge3("a\nc\n", "a\nmine\nc\n", "a\ntheirs\nc\n", L);
        assert_eq!(m.conflicts, 1);
    }
}
