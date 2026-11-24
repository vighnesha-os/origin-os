"""Exercise publication boundaries with real committed Git blobs and hostile input."""

from contextlib import redirect_stdout
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from urllib.parse import unquote, urlsplit
from urllib.error import HTTPError
import xml.etree.ElementTree as ET
from html.parser import HTMLParser

spec = importlib.util.spec_from_file_location("public_index", Path(__file__).with_name("build.py"))
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)


class Links(HTMLParser):
    def __init__(self):
        super().__init__()
        self.links = []
        self.tags = []

    def handle_starttag(self, tag, attrs):
        self.tags.append(tag)
        for name, value in attrs:
            if name == "href":
                self.links.append(value)


class BuildTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "repo"
        self.root.mkdir()
        self.output = Path(self.temp.name) / "site"
        self.run_git("init", "-q")
        self.run_git("config", "user.name", "Index Test")
        self.run_git("config", "user.email", "test@example.invalid")

    def run_git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.root), *args], stderr=subprocess.STDOUT)

    def write(self, path, text):
        file = self.root / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(text, encoding="utf-8")

    def commit(self, paths):
        self.write(builder.MANIFEST, json.dumps({"repository": builder.REPOSITORY, "base_url": builder.BASE_URL,
                   "google_site_verification": "test-verification", "files": paths}))
        self.run_git("add", ".")
        self.run_git("commit", "-qm", "Fixture")

    def build(self):
        with redirect_stdout(io.StringIO()):
            builder.build(self.root, self.output)

    def test_committed_allowlist_only_and_crawl_graph(self):
        hostile = '<script>alert("x")</script><img src=x onerror=alert(1)>'
        self.write("README.md", "Origin overview\n" + hostile)
        self.write("docs/a space & ü.md", "A public specification")
        self.write("unlisted.md", "not approved for publication")
        self.commit(["README.md", "docs/a space & ü.md", "removed.md"])
        self.write("README.md", "uncommitted local content")
        self.write("local.md", "untracked content")
        self.build()
        home = (self.output / "index.html").read_text()
        file_page = (self.output / "files/README.md.html").read_text()
        self.assertIn("test-verification", home)
        self.assertIn("Origin overview", file_page)
        self.assertNotIn("uncommitted local content", file_page)
        self.assertIn("&lt;script&gt;", file_page)
        self.assertFalse((self.output / "files/unlisted.md.html").exists())
        self.assertFalse((self.output / "files/local.md.html").exists())
        self.assertFalse((self.output / "files/removed.md.html").exists())
        seen = set()
        pending = [builder.BASE_URL]
        while pending:
            url = pending.pop()
            if url in seen:
                continue
            seen.add(url)
            path = unquote(url.removeprefix(builder.BASE_URL)).split("#")[0]
            local = self.output / (path + "index.html" if not path or path.endswith("/") else path)
            self.assertTrue(local.is_file(), url)
            if local.suffix == ".html":
                parsed = Links()
                parsed.feed(local.read_text())
                self.assertNotIn("script", parsed.tags)
                self.assertNotIn("img", parsed.tags)
                self.assertNotIn('name="robots" content="noindex"', local.read_text())
                self.assertIn('rel="canonical" href="' + url + '"', local.read_text())
                pending.extend(link for link in parsed.links if link.startswith(builder.BASE_URL) and not link.endswith("sitemap.xml"))
        sitemap = ET.parse(self.output / "sitemap.xml")
        urls = {node.text for node in sitemap.findall(".//{*}loc")}
        self.assertEqual(urls, seen)
        self.assertEqual(len(urls), 4)
        self.assertTrue(all(urlsplit(url).hostname == "vighnesha-os.github.io" for url in urls))
        self.assertIn("noindex", (self.output / "404.html").read_text())

    def test_symlinks_are_refused_without_following_them(self):
        (self.root / "README.md").symlink_to("/etc/passwd")
        self.commit(["README.md"])
        with self.assertRaisesRegex(ValueError, "regular tracked"):
            self.build()
        self.assertFalse(self.output.exists())

    def test_secret_added_to_an_approved_path_stops_the_build(self):
        self.write("README.md", "gh" + "p_" + "A" * 36)
        self.commit(["README.md"])
        with self.assertRaisesRegex(ValueError, "potential secret"):
            self.build()
        self.assertFalse(self.output.exists())

    def test_restricted_paths_even_if_allowlisted(self):
        for path in [".env", "private/notes.md", "secret.txt", "id_rsa", "specifications/reference.md",
                     "boot/uefi/trust/test.txt", "docs/hardware/records/log.md", "a/../../README.md"]:
            with self.subTest(path=path):
                self.assertIsNotNone(builder.path_reason(path))

    def test_binary_oversize_and_lfs_are_refused(self):
        for content in ["binary\0bytes", "x" * (builder.MAX_BYTES + 1), "version https://git-lfs.github.com/spec/v1\n"]:
            self.write("README.md", content)
            self.commit(["README.md"])
            with self.assertRaises(ValueError):
                self.build()
            self.assertFalse(self.output.exists())

    def test_output_is_never_reused(self):
        self.write("README.md", "public")
        self.commit(["README.md"])
        self.output.mkdir()
        (self.output / "stale.html").write_text("stale")
        with self.assertRaisesRegex(ValueError, "new directory"):
            self.build()

    def test_duplicate_and_destination_changes_are_refused(self):
        self.write("README.md", "public")
        self.commit(["README.md", "README.md"])
        with self.assertRaisesRegex(ValueError, "allowlist"):
            self.build()
        self.write(builder.MANIFEST, json.dumps({"repository": "someone/else", "base_url": builder.BASE_URL, "files": ["README.md"]}))
        self.run_git("add", ".")
        self.run_git("commit", "-qm", "Change destination")
        with self.assertRaisesRegex(ValueError, "destination"):
            self.build()

    def test_public_check_requires_anonymous_positive_confirmation(self):
        for metadata in [{"private": True}, {"private": False},
                         {"private": False, "visibility": "public", "full_name": "someone/else"}]:
            with self.subTest(metadata=metadata), patch.object(builder, "urlopen") as opened:
                opened.return_value.__enter__.return_value = io.StringIO(json.dumps(metadata))
                with self.assertRaises(ValueError):
                    builder.check_public()
                self.assertEqual(opened.call_count, 1)
        with patch.object(builder, "urlopen") as opened:
            opened.return_value.__enter__.return_value = io.StringIO(json.dumps({"private": False, "visibility": "public", "full_name": builder.REPOSITORY}))
            builder.check_public()
            request = opened.call_args.args[0]
            self.assertNotIn("Authorization", request.headers)

    def test_rate_limit_fallback_requires_anonymous_public_page(self):
        url = "https://github.com/" + builder.REPOSITORY
        body = ('<html><head><meta name="octolytics-dimension-repository_nwo" content="' + builder.REPOSITORY
                + '"><meta name="octolytics-dimension-repository_public" content="true"></head><body></body></html>')
        for code in [403, 429]:
            response = io.BytesIO(body.encode())
            response.status = 200
            response.geturl = lambda: url
            with self.subTest(code=code), patch.object(builder, "urlopen", side_effect=[HTTPError("api", code, "rate limited", {}, None), response]) as opened:
                with redirect_stdout(io.StringIO()):
                    builder.check_public()
                self.assertEqual(opened.call_count, 2)
                self.assertTrue(all("Authorization" not in call.args[0].headers for call in opened.call_args_list))

    def test_fallback_refuses_missing_private_wrong_redirected_or_duplicate_metadata(self):
        url = "https://github.com/" + builder.REPOSITORY
        identity = '<meta name="octolytics-dimension-repository_nwo" content="' + builder.REPOSITORY + '">'
        public = '<meta name="octolytics-dimension-repository_public" content="true">'
        cases = [
            ("<head></head>", url),
            ("<head>" + identity + public.replace("true", "false") + "</head>", url),
            ("<head>" + identity.replace(builder.REPOSITORY, "someone/else") + public + "</head>", url),
            ("<head>" + identity + public + public + "</head>", url),
            ("<head></head><body>" + identity + public + "</body>", url),
            ("<head>" + identity + public + "</head>", "https://github.com/login"),
            ("<head>" + identity + public + "</head>" + "x" * builder.MAX_BYTES, url),
        ]
        for body, destination in cases:
            response = io.BytesIO(body.encode())
            response.status = 200
            response.geturl = lambda: destination
            with self.subTest(body=body[:120], destination=destination), patch.object(builder, "urlopen", side_effect=[HTTPError("api", 403, "rate limited", {}, None), response]):
                with self.assertRaises(ValueError):
                    builder.check_public()
        with patch.object(builder, "urlopen", side_effect=HTTPError("api", 404, "not found", {}, None)) as opened:
            with self.assertRaises(HTTPError):
                builder.check_public()
            self.assertEqual(opened.call_count, 1)


if __name__ == "__main__":
    unittest.main()
