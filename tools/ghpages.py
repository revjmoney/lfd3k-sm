#!/usr/bin/env python3
"""The gh-pages branch of LFD3K-SM: the store of release images for the AXIS installer.

    python -I tools/ghpages.py release --tag v0.5.0 --from DIR [--into DIR]

--into defaults to $GHP, which tools/ci/ghpages_publish.sh sets to a fresh copy of gh-pages.

The website itself is not on this branch: GitHub Pages deploys it from the Website workflow
(.github/workflows/pages.yml), which copies releases/ from this branch into the site it deploys.
So the AXIS installer finds the images at https://revjmoney.github.io/lfd3k-sm/releases/.

The release workflow adds releases/<tag>/ (one folder per env, each with its images and
manifest.json) and rewrites releases/index.json. Only the three newest releases (by version
number) are kept. Anything else on the branch is left as it is.

Copied from the AXIS Micro BBS repo (tools/ghpages.py, release half), since the repos are separate.
"""
import argparse
import json
import os
import re
import shutil
import sys

KEEP_RELEASES = 3
TAG = re.compile(r"^v[0-9][0-9A-Za-z._-]*$")


def version_key(tag):
    """v0.9 < v0.52 < v0.100: numbers compare as numbers. A tag with anything after its dotted
    numbers ranks below the bare version: v0.6.0-rc1 < v0.6.0 (and above v0.5.9)."""
    m = re.match(r"^v?(\d+(?:\.\d+)*)(.*)$", tag)
    if not m:
        return ((), 0, tag)
    nums = tuple(int(p) for p in m.group(1).split("."))
    return (nums, 0 if m.group(2) else 1, m.group(2))


def releases(gh_pages_dir):
    """Release tags on the branch, newest first."""
    root = os.path.join(gh_pages_dir, "releases")
    if not os.path.isdir(root):
        return []
    tags = [n for n in os.listdir(root) if os.path.isdir(os.path.join(root, n))]
    return sorted(tags, key=version_key, reverse=True)


def write_index(gh_pages_dir):
    tags = releases(gh_pages_dir)
    idx = {"latest": tags[0] if tags else None,
           "previous": tags[1] if len(tags) > 1 else None,
           "releases": tags}
    root = os.path.join(gh_pages_dir, "releases")
    os.makedirs(root, exist_ok=True)
    with open(os.path.join(root, "index.json"), "w", encoding="utf-8", newline="\n") as fh:
        json.dump(idx, fh, indent=1)
        fh.write("\n")
    return idx


def prune(gh_pages_dir, keep_releases=KEEP_RELEASES):
    """Drop releases past the newest keep_releases and rewrite releases/index.json.
    Returns the removed folders (relative paths)."""
    removed = []
    for tag in releases(gh_pages_dir)[keep_releases:]:
        shutil.rmtree(os.path.join(gh_pages_dir, "releases", tag))
        removed.append("releases/" + tag)
    write_index(gh_pages_dir)
    return removed


def add_release(gh_pages_dir, tag, src_dir):
    """Put src_dir at releases/<tag>/ (replacing an earlier copy), prune, rewrite the index."""
    if not isinstance(tag, str) or not TAG.match(tag) or ".." in tag:
        raise ValueError("not a release tag: %r" % (tag,))
    if not os.path.isdir(src_dir):
        raise ValueError("no release folder: %s" % src_dir)
    dst = os.path.join(gh_pages_dir, "releases", tag)
    if os.path.isdir(dst):
        shutil.rmtree(dst)
    shutil.copytree(src_dir, dst)
    return prune(gh_pages_dir)


def main(argv=None):
    ap = argparse.ArgumentParser(prog="ghpages")
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("release")
    r.add_argument("--tag", required=True)
    r.add_argument("--from", dest="src", required=True)
    r.add_argument("--into", default=os.environ.get("GHP", ""))
    args = ap.parse_args(argv)
    if not args.into:
        print("ghpages: no target folder (--into or GHP)", file=sys.stderr)
        return 1
    try:
        gone = add_release(args.into, args.tag, args.src)
        idx = write_index(args.into)
        print("release %s added; latest %s, previous %s" % (args.tag, idx["latest"], idx["previous"]))
        for path in gone:
            print("removed " + path)
    except (OSError, ValueError) as e:
        print("ghpages: %s" % e, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
