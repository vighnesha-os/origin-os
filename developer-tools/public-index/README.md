# Public source and documentation index

Task `PUBLIC-INDEX-2026-11-24`: make the public repository's appropriate files
available as linked static HTML with an XML sitemap, at
<https://vighnesha-os.github.io/origin-os/>. The repository owner requested this
implementation and Pages/Search Console configuration on 2026-11-24.
This host-only publishing tool changes no OS format, runtime dependency, release
version or requirement-completion claim.

## Publication contract

- `files.json` is the reviewed publication allowlist. New files are **not**
  automatically approved. Existing approved files update on pushes to `main`;
  deleted files disappear from a fresh artifact.
- Only regular committed blobs in `HEAD` are read, through Git. Untracked and
  ignored files, local modifications, symlinks, submodules and Git history are
  never published. No source file is run or included as active HTML.
- A type/path policy excludes environment and credential files, hidden local
  directories, key material, raw hardware logs, binary archives and cryptographic
  vector data. A content check refuses recognizable credentials and private-key
  encodings, binary/control characters, LFS pointers and files over 2 MiB.
- The allowlist and filters reduce risk; pattern matching cannot prove that text
  has no secret. Review approved content changes before merging. A refusal stops
  deployment; it does not remove the previously published site. If a secret was
  already public, revoke it and remove it from both GitHub and Pages; do not rely
  on robots rules or a sitemap omission to conceal it.
- The build and the deployment both check current repository visibility through
  GitHub's **unauthenticated** API. If a shared runner receives HTTP 403 or 429,
  it instead checks GitHub's anonymous repository page: HTTP 200 at the exact
  repository URL and unique head metadata naming this repository and marking it
  public are all required. No token is used. Private repositories, redirects,
  missing/duplicate/mismatched metadata and failed checks refuse publication.
  Before making this repository private, disable or
  unpublish Pages separately: skipping a workflow does not remove an older site.
- The artifact contains only generated HTML, a sitemap and `.nojekyll`. It never
  uploads the checkout. Pull requests test/build without Pages write permission;
  deployment runs only from `main`. Actions are pinned by commit. The deploy job
  alone receives `pages: write` and `id-token: write`; no account secret is used.

## Maintenance and verification

Run from the repository root:

```sh
python3 -m unittest discover -s developer-tools/public-index -p 'test_*.py' -v
python3 developer-tools/public-index/build.py --candidates
python3 developer-tools/public-index/build.py --output /tmp/origin-public-site-new
```

`--candidates` lists eligible paths, **not permission to publish them**. Review
each proposed addition to `files.json`. The build uses the committed allowlist
and committed file contents, so commit changes on a branch before previewing.
The output directory must not exist. Python 3.10+ and Git are the only local
dependencies; no package installation is needed. Builds perform no source
compilation and require network access only to confirm public visibility.

Tests exercise the actual Git-blob reader and output: unlisted/untracked/local
files, symlinks, path restrictions, secrets, binary/oversize files, LFS pointers,
HTML injection, URL encoding, sitemap coverage, complete internal-link reachability,
old-artifact refusal and private/unknown repository checks. Two additional tests
exercise the shared-runner API-rate-limit fallback and its refusal cases.

The Pages setting must use **GitHub Actions**. The existing CI and raw Markdown
`docs/` tree are preserved. The public-index workflow runs on every `main` push,
when manually dispatched, and in affected pull requests for validation. Its
publication list is explicit, rather than assuming every future file is safe.
Protect the `github-pages` environment so only `main` can deploy.

## Search Console and crawling

Use a URL-prefix property for <https://vighnesha-os.github.io/origin-os/>, verify
the homepage's `google-site-verification` tag, and submit `sitemap.xml` there.
The token in `files.json` is a public verification tag, not a login credential.
Keep it in deployed HTML to retain verification. Do not try to verify ownership
of `github.com` or of another repository.

Every indexed page has an absolute self-canonical URL, a title, a description,
visible text and ordinary HTML links. There is no JavaScript requirement,
`noindex`, or snippet restriction on the content pages. The 404 page alone uses
`noindex` and is omitted from the sitemap. No artificial modification dates,
ranking claims, AI-specific markup or sitemap-ping requests are generated.

This project cannot set the hostname's root `/robots.txt` through a project
subdirectory. A file at `/origin-os/robots.txt` would not control crawling, so
none is generated. Check <https://vighnesha-os.github.io/robots.txt> after
deployment. A genuine 404 there normally permits crawling; a disallow rule or
server failure must be investigated. Submit the project sitemap directly in
Search Console instead of claiming a project-local robots file advertises it.

Google decides which URLs to crawl, index, canonicalize and show. A sitemap and
an indexing request are hints. GitHub's own file URLs remain under GitHub's
crawling rules. AI Overviews and AI Mode require a page indexed in Google Search
and eligible for a snippet; there is no separate AI-index submission and no
guarantee of inclusion. A published source-file mirror may be treated as a
duplicate of GitHub or as low-value content. Clear, useful documentation is
more likely to help readers than an indiscriminate file dump.

## Provenance and evidence

Author: Codex, 2026-11-24, original host-side Python implementation with the
standard library; MIT OR Apache-2.0. No OS or third-party implementation source
was copied. No crate or shipping dependency was added. Acceptance: publication
boundary tests pass, the static link graph covers the sitemap, the Pages
deployment succeeds, and public HTTP responses contain the intended HTML and
sitemap. Search Console verification/submission is separate evidence from
actual indexing. No person has reviewed this implementation yet.

The first Pages build and its retry on 2026-11-24 passed the eight original tests
but stopped at GitHub's anonymous API rate limit (HTTP 403). The fallback above
was added for that observed failure; all ten tests pass, including rejection of
private, missing, duplicate, redirected and mismatched page metadata. The public
metadata fields were inspected on GitHub's anonymous repository page that day.

Official documentation consulted on 2026-11-24:

- [GitHub Pages publishing sources](https://docs.github.com/en/pages/getting-started-with-github-pages/configuring-a-publishing-source-for-your-github-pages-site)
- [Custom Pages workflows](https://docs.github.com/en/pages/getting-started-with-github-pages/using-custom-workflows-with-github-pages)
- [Google sitemap construction/submission](https://developers.google.com/search/docs/crawling-indexing/sitemaps/build-sitemap)
- [Google robots.txt scope](https://developers.google.com/crawling/docs/robots-txt/robots-txt-spec)
- [Google AI features and websites](https://developers.google.com/search/docs/appearance/ai-features)
