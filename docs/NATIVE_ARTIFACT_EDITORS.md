# Native document and sheet editing

Kindred-created Markdown documents now enter a rich-text editor instead of the
source textarea. The editor uses a locally bundled Tiptap/ProseMirror engine,
loaded only when editing a document. It has headings, font family/size, bold,
italic, underline, lists, alignment, links, basic tables, color and undo/redo.
The same editor is available from a chat card and the Artifacts workbench.

A save converts the native document to HTML marked `kindred-document-v1`, retaining
its artifact identity, metadata and unrelated shared state. Its structured editor
model and rendered HTML are stored together in `state.kindredDocument`. Preview
uses the existing isolated artifact frame; importing/pasting HTML into the
editor is sanitized and then constrained by the editor schema. Custom executable
HTML/JSX applications retain their source editor, rather than being flattened
into a rich document.

DOCX export recognizes the marked document and exports the saved structured
model, including basic text styles, headings, alignment, lists and tables. It
only uses that model when its saved HTML still matches the source. If a source
edit invalidates the model, export falls back to the current HTML instead of
silently downloading stale text. This remains a basic exporter, not Word layout
round-tripping: embedded images, merged-cell geometry and live hyperlink
relationships are not implemented by this exporter.

Native sheets (including the previous built-in template) open a grid with column
letters, row numbers, a cell-value bar, per-cell font/size/bold/italic/alignment/
color, row and column addition, cell clearing, undo/redo and TSV range paste.
Rows and formatting stay in shared state. Cell values are literal text; this
change does not introduce a formula engine. Existing sheet HTML snapshot export
remains available; XLSX editing/import is not part of this change.

The editor uses explicit Save changes and existing expected-revision checks.
Conflicting or failed saves retain the draft; Load latest confirms before
replacing it. A new document/sheet opens directly in editing mode. Closing or
navigating away still checks unsaved changes. Formatted document metadata has a
256 KB bound; unrelated shared state retains its 32 KB bound and source retains
its existing 64 KB bound. An oversized save reports an error rather than trimming
content.

Uploaded DOCX/XLSX previews are unchanged. This fixes native Kindred artifacts,
not the separate office-file preview/editing problem.

Checks: test-artifact-editors.cjs (Chromium/WebKit and mobile-width layout),
test-artifact-studio.cjs (full app including save/rename/conflict/custom apps),
artifact_export tests, and workspace_artifacts tests. Actual iOS keyboard/IME
acceptance remains a Mac/iPhone task; WebKit is not a native-device substitute.
