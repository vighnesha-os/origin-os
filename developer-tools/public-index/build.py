#!/usr/bin/env python3
"""Build a static, reviewed public-file index using Python's standard library.

Only regular blobs in HEAD and files.json are read. No source content is executed,
no working-tree file is copied, and publication requires a live public-repo check.
"""

import argparse
from collections import Counter
import html
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
from urllib.parse import quote, urlsplit
from urllib.request import Request, urlopen
import xml.etree.ElementTree as ET

REPOSITORY = "vighnesha-os/origin-os"
BASE_URL = "https://vighnesha-os.github.io/origin-os/"
MANIFEST = "developer-tools/public-index/files.json"
MAX_BYTES = 2 * 1024 * 1024
EXTENSIONS = {".rs", ".md", ".toml", ".txt", ".py", ".sh", ".yml", ".yaml", ".oidl", ".in", ".lock", ".json"}
NAMES = {"CODEOWNERS", "LICENSE-MIT", "LICENSE-APACHE", ".gitignore", "rustc-commit-hash"}
BLOCKED_PARTS = {"private", "secrets", "credentials", ".git", ".ssh", ".aws", ".gnupg", "specifications", "node_modules", "target"}
BLOCKED_PREFIXES = ("boot/uefi/trust/", "security/crypto/tests/vectors/", "security/random/src/testdata/", "docs/hardware/records/")
SECRET_PATTERNS = [
    re.compile(r"-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY-----"),
    re.compile(r"\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,})\b"),
    re.compile(r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b"),
    re.compile(r"\bAIza[0-9A-Za-z_-]{35}\b"),
    re.compile(r"\bsk-(?:proj-|ant-)?[A-Za-z0-9_-]{32,}\b"),
    re.compile(r"https?://[^\s/:<>]+:[^\s/@<>]+@", re.I),
    re.compile(r'''(?im)^\s*["']?(?:api[_-]?key|access[_-]?token|client[_-]?secret|password)["']?\s*[:=]\s*["'][A-Za-z0-9_+/=-]{20,}["']'''),
]
STYLE = """body{font:17px/1.6 system-ui,sans-serif;margin:0;color:#172538;background:#f7f9fc}
header,main,footer{max-width:1100px;margin:auto;padding:24px}header{border-bottom:1px solid #d8dfe9}
a{color:#1454ad;text-underline-offset:3px}a:focus-visible{outline:3px solid #e49e00;outline-offset:3px}
h1{font-size:clamp(1.5rem,4vw,2.5rem);line-height:1.2;overflow-wrap:anywhere}h2{font-size:1.3rem}
nav{display:flex;gap:20px;flex-wrap:wrap}pre{background:white;border:1px solid #d8dfe9;padding:20px;
font:14px/1.6 ui-monospace,monospace;white-space:pre-wrap;overflow-wrap:anywhere;tab-size:4}
ul{padding-left:24px}li{margin:6px 0;overflow-wrap:anywhere}.muted,footer{color:#526074;font-size:14px}
.skip{position:absolute;top:-100px}.skip:focus{top:0;background:white;padding:10px}
"""


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args])


def check_public():
    """Use the unauthenticated API; a token must never make a private repo eligible."""
    request = Request("https://api.github.com/repos/" + REPOSITORY,
                      headers={"Accept": "application/vnd.github+json", "User-Agent": "origin-public-file-index"})
    with urlopen(request, timeout=30) as response:
        metadata = json.load(response)
    if metadata.get("private") is not False or metadata.get("visibility") != "public" or metadata.get("full_name") != REPOSITORY:
        raise ValueError("Publication refused: repository is not confirmed public")


def entries(root):
    result = {}
    for record in git(root, "ls-tree", "-rz", "--full-tree", "HEAD").split(b"\0"):
        if record:
            meta, raw_path = record.split(b"\t", 1)
            mode, kind, sha = meta.decode("ascii").split()
            result[raw_path.decode("utf-8")] = (mode, kind, sha)
    return result


def path_reason(path):
    p = PurePosixPath(path)
    parts = path.split("/")
    if not path or p.is_absolute() or any(x in {"", ".", ".."} for x in parts) or "\\" in path or any(ord(x) < 32 for x in path):
        return "unsafe path"
    if any(x.lower() in BLOCKED_PARTS for x in parts):
        return "restricted path"
    if path.startswith(BLOCKED_PREFIXES):
        return "key material, raw logs or test data"
    name = p.name.lower()
    if name.startswith(".env") or re.search(r"(?:secret|credential|private[-_]key|id_rsa|id_ed25519)", name):
        return "sensitive filename"
    if any(x.startswith(".") for x in parts) and parts[0] not in {".github", ".cargo", ".gitignore"}:
        return "hidden path"
    if p.suffix.lower() not in EXTENSIONS and p.name not in NAMES:
        return "unsupported type"
    return None


def read_safe(root, path, entry):
    reason = path_reason(path)
    if reason:
        raise ValueError(reason)
    mode, kind, sha = entry
    if kind != "blob" or mode not in {"100644", "100755"}:
        raise ValueError("not a regular tracked file")
    if int(git(root, "cat-file", "-s", sha)) > MAX_BYTES:
        raise ValueError("oversize file")
    raw = git(root, "cat-file", "blob", sha)
    text = raw.decode("utf-8")
    if any(ord(c) < 32 and c not in "\t\r\n" for c in text):
        raise ValueError("binary or control characters")
    if text.startswith("version https://git-lfs.github.com/spec/v1"):
        raise ValueError("Git LFS pointer")
    if any(pattern.search(text) for pattern in SECRET_PATTERNS):
        raise ValueError("potential secret; content withheld")
    return text


def file_url(path):
    return BASE_URL + "files/" + quote(path, safe="/") + ".html"


def tree_url(path):
    return BASE_URL + "tree/" + quote(path, safe="/") + "/"


def page(title, description, url, body, revision, verification="", noindex=False):
    esc = html.escape
    tag = f'<meta name="google-site-verification" content="{esc(verification, quote=True)}">' if verification else ""
    robots = '<meta name="robots" content="noindex">' if noindex else ""
    return f'''<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{esc(title)} · Origin OS</title><meta name="description" content="{esc(description, quote=True)}">
<link rel="canonical" href="{esc(url, quote=True)}"><link rel="sitemap" type="application/xml" href="{BASE_URL}sitemap.xml">
{tag}{robots}<style>{STYLE}</style></head><body><a class="skip" href="#main">Skip to content</a>
<header><nav aria-label="Main navigation"><a href="{BASE_URL}">Origin OS file index</a>
<a href="https://github.com/{REPOSITORY}">GitHub repository</a><a href="{BASE_URL}sitemap.xml">Sitemap</a></nav></header>
<main id="main">{body}</main><footer>Public files from the main branch · Source revision
<a href="https://github.com/{REPOSITORY}/commit/{revision}">{revision[:12]}</a>.
Origin OS is in early development. Source and documentation retain their original licences.</footer></body></html>\n'''


def build(root, output):
    """Render only committed allowlisted blobs. The caller checks public visibility."""
    inventory = entries(root)
    manifest = json.loads(git(root, "show", "HEAD:" + MANIFEST))
    if manifest.get("repository") != REPOSITORY or manifest.get("base_url") != BASE_URL:
        raise ValueError("Unexpected publishing destination")
    paths = manifest["files"]
    if not isinstance(paths, list) or not paths or not all(isinstance(p, str) for p in paths) or len(paths) != len(set(paths)):
        raise ValueError("Invalid publication allowlist")
    texts = {}
    removed = 0
    # Validate all content before creating any artifact; never print matching values.
    for path in sorted(paths):
        if path not in inventory:
            removed += 1
            continue
        try:
            texts[path] = read_safe(root, path, inventory[path])
        except (ValueError, UnicodeError) as error:
            raise ValueError(f"Publication refused for {path}: {error}") from None
    if not texts:
        raise ValueError("No eligible files")
    if output.exists():
        raise ValueError("Output must be a new directory; old artifacts must not be reused")
    output.mkdir(parents=True)
    revision = git(root, "rev-parse", "HEAD").decode().strip()
    verification = manifest.get("google_site_verification", "")
    if not re.fullmatch(r"[A-Za-z0-9_-]*", verification):
        raise ValueError("Invalid verification tag")
    esc = html.escape
    urls = [BASE_URL]
    directories = set()
    for path, text in texts.items():
        p = PurePosixPath(path)
        directories.update(str(parent) for parent in p.parents if str(parent) != ".")
        url = file_url(path)
        source = f"https://github.com/{REPOSITORY}/blob/{revision}/{quote(path, safe='/')}"
        parent = str(p.parent)
        back = BASE_URL if parent == "." else tree_url(parent)
        body = f'<p><a href="{esc(back)}">Browse containing folder</a></p><h1>{esc(path)}</h1>'
        body += f'<p class="muted">Public repository file · <a href="{esc(source)}">View this revision on GitHub</a></p>'
        body += '<pre>' + esc(text) + '</pre>'
        target = output / "files" / (path + ".html")
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(page(path, f"Read {path}, a public source or documentation file in Origin OS.", url, body, revision), encoding="utf-8")
        urls.append(url)

    def listing(folder):
        direct_dirs = sorted(d for d in directories if str(PurePosixPath(d).parent) == (folder or "."))
        direct_files = sorted(p for p in texts if str(PurePosixPath(p).parent) == (folder or "."))
        links = [f'<li><a href="{esc(tree_url(d))}">{esc(PurePosixPath(d).name)}/</a></li>' for d in direct_dirs]
        links += [f'<li><a href="{esc(file_url(p))}">{esc(PurePosixPath(p).name)}</a></li>' for p in direct_files]
        return "<ul>" + "\n".join(links) + "</ul>"

    for folder in sorted(directories):
        target = output / "tree" / folder / "index.html"
        target.parent.mkdir(parents=True, exist_ok=True)
        body = f'<h1>{esc(folder)}/</h1>' + listing(folder)
        target.write_text(page(folder + "/", f"Browse public files in {folder} in Origin OS.", tree_url(folder), body, revision), encoding="utf-8")
        urls.append(tree_url(folder))
    body = '''<h1>Origin OS: source and documentation</h1>
<p>Origin OS is an original-source x86-64 operating system written in Rust. Its kernel is the Framekernel.
Origin Desktop and Origin Server are planned editions sharing one kernel and native SDK. The project is in early development.</p>
<p>Explore the public source, architecture decisions, specifications, requirements and engineering notes below.
These static pages work without JavaScript and link to the exact source revision on GitHub.</p>'''
    body += "<h2>Start here</h2><ul>" + "".join(f'<li><a href="{file_url(p)}">{esc(p)}</a></li>' for p in ["README.md", "docs/PROGRESS.md", "docs/ROADMAP.md", "CONTRIBUTING.md", "SECURITY.md"] if p in texts) + "</ul>"
    body += f"<h2>Browse public files</h2><p>{len(texts)} reviewed text files. New files require a publication-list review; key material, raw logs, binary archives and test vectors are excluded.</p>" + listing("")
    (output / "index.html").write_text(page("Source and documentation", "Browse Origin OS public Rust source, Framekernel documentation, architecture decisions and native SDK specifications.", BASE_URL, body, revision, verification), encoding="utf-8")
    (output / "404.html").write_text(page("File not found", "This public file page does not exist.", BASE_URL + "404.html", '<h1>File not found</h1><p><a href="' + BASE_URL + '">Browse the current file index</a>.</p>', revision, noindex=True), encoding="utf-8")
    if len(urls) > 50000:
        raise ValueError("Sitemap URL limit exceeded; split the sitemap before publishing")
    ET.register_namespace("", "http://www.sitemaps.org/schemas/sitemap/0.9")
    sitemap = ET.Element("{http://www.sitemaps.org/schemas/sitemap/0.9}urlset")
    for url in urls:
        node = ET.SubElement(sitemap, "{http://www.sitemaps.org/schemas/sitemap/0.9}url")
        ET.SubElement(node, "{http://www.sitemaps.org/schemas/sitemap/0.9}loc").text = url
    ET.ElementTree(sitemap).write(output / "sitemap.xml", encoding="utf-8", xml_declaration=True)
    (output / ".nojekyll").touch()
    print(f"Built {len(texts)} file pages, {len(directories)} folder pages and {len(urls)} sitemap URLs; {removed} removed allowlist entries skipped.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--candidates", action="store_true", help="List eligible tracked paths for manual publication-list review; does not publish")
    parser.add_argument("--check-public", action="store_true", help="Only verify the repository is still public")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    if args.candidates:
        candidates = []
        omitted = Counter()
        for path, entry in sorted(entries(root).items()):
            try:
                read_safe(root, path, entry)
                candidates.append(path)
            except (ValueError, UnicodeError) as error:
                omitted[str(error)] += 1
        print(json.dumps(candidates, indent=2))
        print("Omission counts: " + json.dumps(omitted), file=__import__("sys").stderr)
        return
    check_public()
    if not args.check_public:
        if args.output is None:
            parser.error("--output is required")
        build(root, args.output.resolve())


if __name__ == "__main__":
    main()
