# Import and export

Most of the resource lists in the dashboard carry an **Export** action, and
the lists you build up over time also carry an **Import** action. Both run
entirely in the browser against the rows the view already holds, so there is
no separate bulk API and no server round-trip: export downloads a file from
what is on screen, and import replays your file as ordinary create calls.

## Export

The **Export** button opens a lightbox where you pick which columns to
include and a file format, then download the file:

| Format | What you get |
|---|---|
| **CSV** | A comma-separated file with a header row. The values are quoted only where a comma, quote or newline would otherwise break the row. |
| **JSON** | An array of objects, one per row, keyed by the column. This suits records with long or structured values such as HTML bodies. |
| **Excel (.csv)** | The same CSV with a UTF-8 byte-order mark, so Excel opens it as UTF-8 rather than the local codepage. |

Every column is selected by default; clear the ones you do not want, or use
**Clear all** / **Select all**. The file is named after the resource and
downloads straight from the browser. Export reflects the rows currently
loaded in the list.

Exports never include secrets. Credential export, for example, carries the
metadata (name, type, status, usage) but never the secret key, which is
shown only once at creation.

## Import

The **Import** button opens a lightbox that walks you through a CSV upload:

1. **Download template** produces a CSV with the exact header row for the
   resource plus one example row, so you fill in the right columns.
2. **Choose CSV file** parses your file in the browser. The parser follows
   RFC 4180 (quoted fields, escaped `""` quotes, embedded commas and
   newlines, CRLF or LF). The header row is matched case-insensitively
   against the known columns (the column key or its human label); unknown
   columns are ignored, and blank rows or rows missing a required field are
   dropped.
3. A **preview** shows the first few parsed rows and the total count.
4. **Import** creates the rows one at a time through the resource's normal
   create API, so each row goes through the same validation as a
   hand-created record. A progress bar tracks the run.

Import tolerates per-row failures: a row that the API rejects is counted as
failed and the rest continue. The result toast reports how many rows were
imported and how many failed, with the first error message for context.

## Where it is available

| List | Export | Import |
|---|---|---|
| Domains | yes | yes |
| Credentials | yes (metadata only) | no |
| Routes | yes | yes |
| Webhooks | yes | yes |
| Suppressions | yes | yes |
| Recipients | yes | no |

Broadcast **subscribers** have their own import on the stream's
Subscribers tab (an add box, a paste-to-import textarea and a CSV upload),
described in [Broadcast streams](broadcast.md).
