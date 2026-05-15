# Changelog

All notable changes to `mcp-advwiki` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Marked roadmap items 1-3 as implemented in `roadmap.md` and `roadmap-pt.md`
  (navigable `index.md`, YAML frontmatter, and a strengthened `lint_wiki`),
  with a per-item status note describing the delivered tools and remaining gaps.

## [0.1.6] - 2026-05-15

This release covers roadmap phase 1: the foundation for a structured,
navigable wiki.

### Added

- **YAML frontmatter support** (`frontmatter` module): parsing and manipulation
  of structured page metadata — `type`, `project`, `status`, `created_at`,
  `updated_at`, `confidence`, `sources`, `related`, `tags`, and `owner` —
  including automatic date updates on write.
- **`lint_wiki` module**: structural and quality checks with `quick` and `all`
  scopes, detecting broken links, orphan pages, missing frontmatter, raw
  sources without a derived page, stale pages, decisions without rationale,
  and duplicate/similar pages.
- **Page classification tools**: `list_pages_by_type`, `list_pages_by_project`,
  and `list_pages_by_tag` filter pages by their frontmatter fields.
- **`find_pages_without_sources`**: lists pages with no `sources` entry in the
  frontmatter, surfacing candidates for review or linking to raw sources.
- **`rebuild_wiki_index`**: generates the navigable wiki index page
  (`wiki://page/index`), grouping pages by type and project from the
  frontmatter — distinct from the raw-source `rawindex.md`.

### Changed

- Refactored the `lint_wiki` tool: streamlined logic and an updated report
  format.
- Expanded README documentation and guidance for the root and global indices.

## [0.1.5] - 2026-05-14

### Added

- Exposed the `delete_page` and `delete_raw_source` tools.

### Fixed

- Fixed a bug in `delete_raw_source`.

### Changed

- Aligned the README with the real API.

## [0.1.4] - 2026-05-11

### Added

- Portuguese roadmap file (`roadmap-pt.md`).

### Changed

- Clarified documentation strings in `main.rs` and adjusted configuration
  example paths.

## [0.1.3] - 2026-05-11

### Changed

- Moved the npm publish logic into a dedicated workflow file and improved the
  npm publish workflow (Node.js v24, better artifact handling and tarball
  inspection).

## [0.1.2] - 2026-05-11

### Changed

- Release workflow adjustments for npm publishing.

## [0.1.1] - 2026-05-11

### Changed

- README adjustments for the npm install command.

## [0.1.0] - 2026-05-09

### Added

- Initial release: a local MCP wiki server running over `stdio` with
  JSON-RPC 2.0, backed by Tantivy BM25 full-text search.
- Core tools: `query_wiki`, `update_page`, `ingest_source`,
  `ingest_extracted_content`, `lint_wiki`, and `read_knowledge_uri`.
- GitHub release workflow with cargo-dist and an npm installer.

[Unreleased]: https://github.com/dfalci/mcp-advwiki/compare/v0.1.6...HEAD
[0.1.6]: https://github.com/dfalci/mcp-advwiki/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/dfalci/mcp-advwiki/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/dfalci/mcp-advwiki/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/dfalci/mcp-advwiki/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/dfalci/mcp-advwiki/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/dfalci/mcp-advwiki/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/dfalci/mcp-advwiki/releases/tag/v0.1.0
