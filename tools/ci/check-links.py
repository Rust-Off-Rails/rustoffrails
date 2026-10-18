#!/usr/bin/env python3
"""Fail if any relative Markdown link or #anchor in the repo doesn't resolve (GitHub-style heading slugs)."""
import glob, os, re, sys

def slug(h):
    return re.sub(r"[^\w\- ]", "", h.strip().lower()).replace(" ", "-")

files = [f for f in glob.glob("**/*.md", recursive=True) if not f.startswith(("evals/results", "node_modules", "template/app/", "template/parts/"))]
# template/app and template/parts are app files: their links resolve inside an app (.offrails/...), not here.
anchors = {}
for f in files:
    anchors[f], in_code = set(), False
    for line in open(f, encoding="utf-8"):
        if line.startswith("```"):
            in_code = not in_code
        if not in_code and line.startswith("#"):
            anchors[f].add(slug(line.lstrip("#")))
        if not in_code:
            # An explicit anchor kept for links to a renamed heading: <a id="old-slug"></a>
            anchors[f].update(re.findall(r'<a id="([^"]+)"></a>', line))

bad = []
for f in files:
    for m in re.finditer(r"\]\(([^)\s]+)\)", open(f, encoding="utf-8").read()):
        target = m.group(1)
        if target.startswith(("http://", "https://", "mailto:")):
            continue
        path, _, anchor = target.partition("#")
        dest = os.path.normpath(os.path.join(os.path.dirname(f), path)) if path else f
        if not os.path.exists(dest):
            bad.append(f"{f}: {target} (missing file)")
        elif anchor and dest.endswith(".md") and anchor not in anchors.get(dest, set()):
            bad.append(f"{f}: {target} (missing anchor)")

for b in bad:
    print(b)
print(f"{len(files)} files checked, {len(bad)} broken link(s)")
sys.exit(1 if bad else 0)
