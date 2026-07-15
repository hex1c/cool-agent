<!-- markdownlint-disable MD013 -->

# Version 1 Quotation Layout Contract

**Status:** Layout and overflow contract approved on 2026-07-14; renderer prototype, visual overlay, and golden PDF pending
**Task:** Phase 0, Task 4
**Reference:** `docs/sample-quotation.pdf`

## Evidence and limitations

The immutable reference currently has SHA-256:

```text
1c11d46b7fc9552253ed4a3ba71368219061f378ab904dbb83db10e2493a56a8
```

The initial measurements below come from the PDF page dictionary, content stream,
font resources, text matrices, vector paths, and image matrices. They have not
yet been approved through a rendered visual overlay because no PDF rasterizer is
installed in the development environment. Do not treat coordinate extraction
alone as visual approval.

The reference contains sample company, customer, banking, tax, and contact data.
This contract records field roles and geometry rather than copying those values.
The later golden fixture must use synthetic data.

## Coordinate system

All contract coordinates use PDF points with a **top-left origin**, positive X to
the right, and positive Y downward. One point is 1/72 inch.

| Property | Value |
| --- | ---: |
| Page count | 1 |
| Width | 595.280029 pt |
| Height | 841.890015 pt |
| Nominal size | A4 portrait |
| User unit | 1 |
| Rotation | 0 |
| Outer content margin | 24 pt |
| Main content width | 547.280029 pt |

The source content stream begins with a Y-axis flip. Renderer code must expose the
top-left contract above and hide backend-specific bottom-left transforms.

## Source resources

### Fonts

The source embeds subsets of these fonts:

| PDF resource | Embedded base font | Observed sizes | Use |
| --- | --- | --- | --- |
| F2 | SFUIDisplay-Semibold | 7.1, 7.2, 7.5, 8.1, 8.8, 9.0 pt | Labels, table headings, footer |
| F3 | SFUIDisplay-Bold | 7.9, 8.1, 9.9, 12.0, 12.6 pt | Titles, emphasized values, totals |
| F9 | SFUIText-Regular | 6.6, 7.2, 7.5, 8.0, 8.1, 8.8 pt | Addresses, body values, terms |
| F1 | Helvetica | Declared; no text placement observed in the extracted stream | Fallback resource only |

The subset binaries in the reference do not establish redistribution rights for
renderer source assets. Before implementation, a human must supply approved,
embeddable font files or approve a metrically compatible substitute. A
substitution is accepted only after structural and visual comparison; renderer
code must never silently use a system font.

### Colors

| Role | RGB | Hex |
| --- | --- | --- |
| Primary dark text/border | 29, 29, 31 | `#1D1D1F` |
| Secondary dark text | 51, 51, 51 | `#333333` |
| Quotation accent | 39, 110, 241 | `#276EF1` |
| Watermark opacity | Approximately 10% | Pending visual confirmation |

### Images and reserved slots

| Slot | X | Y | Width | Height | Source pixels | Interpretation |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| Company logo | 29.50 | 47.87 | 72.00 | 71.64 | 201 × 200 | Configuration-driven logo |
| Signature | 466.78 | 430.73 | 99.00 | 60.55 | 327 × 200 | Configuration-driven signature; final asset pending |
| Footer brand image | 483.76 | 796.89 | 87.52 | 27.00 | 671 × 207 | Source-vendor branding; retention requires approval |
| Background watermark | 47.64 | 158.95 | 500.00 | 500.00 | 1281 × 1281 | Low-opacity decorative source image |

The footer contains a link annotation to the source invoicing vendor. Version 1
must explicitly decide whether to reproduce or remove third-party branding and
its hyperlink; it must not be copied accidentally.

## Page regions

| Region | X | Y | Width | Height | Required content |
| --- | ---: | ---: | ---: | ---: | --- |
| Top title strip | 24.00 | 24.00 | 547.28 | 19.37 | Centered `QUOTATION`; recipient-copy label at right |
| Company identity | 29.50 | 47.87 | 242.50 | 72.70 | Logo, company name, GSTIN, address, contact |
| Quotation metadata | 302.14 | 51.00 | 263.64 | 58.00 | Number, date, place of supply, validity |
| Customer details | 29.50 | 132.00 | 242.50 | 83.50 | Customer label/name, tax ID, billing address |
| Shipping/dispatch | 302.14 | 117.00 | 263.64 | 94.00 | Shipping address and dispatch origin |
| Line-item table | 24.00 | 217.97 | 547.28 | 118.36 | Header and item rows |
| Item/quantity footer | 24.00 | 336.33 | 547.28 | 12.50 | Total item count and quantity |
| Tax and total summary | 390.34 | 348.33 | 180.94 | 55.00 | Taxable amount, tax rows, grand total |
| Amount in words | 24.00 | 403.33 | 547.28 | 11.62 | Currency amount rendered in words |
| Bank/signature panel | 24.00 | 414.23 | 547.28 | 100.85 | Bank details, company signatory label and signature |
| Terms heading/body | 276.17 | 523.00 | 295.11 | 170.00 | Numbered terms with 9.36 pt baseline spacing |
| Footer | 24.00 | 794.07 | 547.28 | 29.82 | Page count, digital-signature note, optional branding |

Region values derived from text and path anchors are rounded to two decimals in
the prose but retained with more precision in
`tests/fixtures/quotation/layout-v1.json`.

## Line-item table

The table outer bounds are X 24.00–571.28 pt. The source header occupies roughly
Y 218.97–234.62 pt and the item body ends at Y 336.33 pt.

Provisional column boundaries:

| Column | Left X | Right X | Alignment | Content |
| --- | ---: | ---: | --- | --- |
| Sequence | 24.00 | 48.23 | Left | 1-based item number |
| Item | 48.23 | 202.09 | Left | Description |
| HSN/SAC | 202.09 | 256.59 | Center | Tax classification |
| Rate/item | 256.59 | 329.29 | Right | Unit rate |
| Quantity | 329.29 | 383.81 | Right | Quantity and unit |
| Taxable value | 383.81 | 444.39 | Right | Pre-tax line amount |
| Tax amount | 444.39 | 511.03 | Right | Tax amount and rate |
| Amount | 511.03 | 571.28 | Right | Post-tax line total |

These boundaries are inferred from header centers, body text starts, and empty
right-alignment anchors because the source does not draw every internal vertical
rule. The visual overlay must confirm or correct them before renderer
implementation. No financial column may be merged or truncated merely to fit
this draft.

Observed text inset is approximately 5.5 pt. The source exposes seven single-line
item baselines on page 1: approximately 244.13, 260.06, 274.19, 288.32, 302.45,
316.58, and 330.71 pt. Empty source rows are visual capacity, not required blank
records.

## Field contract

The renderer receives configuration and confirmed quotation data for:

- Company name, tax ID, postal address, contact details, logo, signature, and
  bank fields.
- Copy label.
- Quotation number, quotation date, place of supply, validity date/period, and
  currency.
- Customer legal/display name, tax ID, billing address, shipping address, and
  dispatch origin.
- Line-item sequence, description, HSN/SAC, quantity, unit, unit rate, taxable
  amount, tax rate/amount, and line total.
- Item count, aggregate quantity, taxable total, named tax components, grand
  total, and amount in words.
- Numbered terms and signatory label.
- Page number and total page count.

Every monetary value must come from the confirmed versioned contract. The PDF
adapter formats values but does not recompute or alter AI-calculated totals.

## Formatting rules

- Currency is explicit in metadata and amount-in-words output.
- Numeric table and summary values align on their decimal/right edge.
- Preserve two decimal places for money in the Version 1 normal case.
- Preserve tax labels and rates separately from tax amounts.
- Dates use one approved locale format consistently after the application
  resolves an ISO calendar date.
- Address lines wrap only within their region and never overlap adjacent fields.
- Missing optional fields collapse according to an approved field map; required
  fields fail rendering with a typed error.
- Company and customer data remain configuration/workflow inputs, never hard-
  coded source-PDF values.

## Quotation number and date semantics

Generate one predictable quotation number per workflow from versioned company
configuration. The default shape is:

```text
<PREFIX>-<FINANCIAL_YEAR>-<ZERO_PADDED_SEQUENCE>
```

Configuration defines the prefix, financial-year/calendar-year scope, sequence
width, and starting value. Reserve the number atomically when the first complete
quotation preview is created. Persist it on the workflow and reuse it for every
correction, confirmation, render retry, Drive copy, and continuation page.
Cancelled or expired reservations may leave gaps, but a number is never reused.
Development and staging numbers require an environment marker so they cannot be
mistaken for production quotations.

The AI may extract either an explicit custom quotation date or the user's intent
to use “today.” It must not invent today's date from model knowledge. The
application supplies the trusted current date and company timezone in the AI
request. Date resolution is:

1. If the user supplied a custom date, the AI returns that date as ISO `YYYY-MM-DD`
   with source attribution; the preview requires human confirmation.
2. If the user requested today or supplied no date, the application uses its
   trusted clock in the configured company timezone.
3. The resolved ISO date is persisted with the quotation revision before preview.
   Version 1 renders it as `DD MMM YYYY` using English month abbreviations, matching
   the reference layout.
4. Corrections may change the date only by creating a new preview revision; they
   do not change the reserved quotation number.

Every page repeats the same persisted quotation number and resolved date. Neither
value is regenerated during rendering or retry.

## Approved overflow direction, pending renderer visual sign-off

The plan already decides that overflow continues to additional pages without
shrinking or truncating financial data. The deterministic policy approved for the
renderer prototype is:

1. Repeat the title strip, persisted quotation number/date, page number, and
   complete table header on every continuation page.
2. Use a minimum item-row line height of 14.13 pt and the reference font sizes.
3. Wrap descriptions inside the Item column; each wrapped line consumes another
   row line. Do not reduce font size to fit.
4. Keep an item together when it fits on the next page. If one description alone
   exceeds a page, split it at a line boundary, label the continuation, and
   repeat its sequence/HSN context without repeating financial amounts until the
   final fragment.
5. Place item/quantity totals, tax summary, grand total, and amount in words only
   after the final line item. If the full financial summary cannot fit, move it
   intact to a new final page.
6. Keep bank/signature content together. Move it intact rather than overlapping
   the table or terms.
7. Flow numbered terms onto following pages at paragraph/line boundaries. Repeat
   the Terms heading and mark continuations.
8. Never split a currency amount, tax row, signature slot, or amount-in-words
   line across pages.
9. Compute total page count before final footer rendering.

The project owner approved the continuation-page composition, pathological
single-item policy, repeated metadata, quotation numbering, and date behavior on
2026-07-14. Third-party footer branding and visual fidelity remain prototype
approval items.

## Golden and overlay verification

`tests/fixtures/quotation/layout-v1.json` defines the sanitized normal case and
structural anchors. `tests/fixtures/quotation/golden-v1.pdf` must be generated by
the selected renderer after fonts and image assets are approved; it must not be a
copy of the source PDF.

Proposed comparison gates:

- Exact page geometry and page count.
- Anchor/border displacement no greater than 0.75 pt.
- Text-baseline displacement no greater than 1.0 pt.
- No clipped glyph, overlapping region, missing financial field, or substituted
  unapproved font.
- Raster overlay at an approved DPI with masked dynamic text regions and a
  separately reviewed full-page difference image.
- Structural extraction confirms labels, monetary strings, and page numbers.

The tolerance, rasterizer, DPI, color threshold, and masked regions require human
approval before becoming test assertions.

## Renderer selection gate

The plan names `printpdf` as the default candidate, but adding it is dependency-
gated. Task 4 cannot select a renderer until the candidate:

- Builds for Rust 1.97 and the `provided.al2023` Lambda target through the Task 3
  SAM path.
- Embeds approved fonts and images deterministically.
- Implements the top-left coordinate adapter and multipage overflow policy.
- Produces a structurally valid PDF and passes the approved overlay tolerance.
- Meets approved Lambda memory, package-size, duration, and cold-start limits.
- Has reviewed licensing and maintained-version evidence.

No renderer dependency has been added by this draft.

## Manual overlay procedure

1. Render the sanitized normal case with field-map guides enabled.
2. Rasterize source and output at the same approved DPI and color profile.
3. Align by page box, not by content guesses.
4. Overlay borders, region boxes, baselines, logo/signature slots, totals, and
   footer anchors.
5. Export a difference image and a guide-only overlay outside the golden PDF.
6. Correct the provisional rightmost table columns and any path/text discrepancy.
7. Repeat for maximum-line, wrapping, and multipage fixtures.
8. Record tool versions, renderer version, font hashes, image hashes, and result.

## Approval

- [x] Layout, numbering, date, and overflow contract approved for implementation.
- [ ] Visual overlay confirms all page-region and table coordinates.
- [ ] Fonts/assets and substitution policy are approved.
- [ ] Third-party footer branding decision is recorded.
- [ ] Rightmost financial table columns are resolved without truncation.
- [x] Multipage and pathological-description behavior is approved.
- [ ] Sanitized normal fixture is approved.
- [ ] Renderer passes SAM build/runtime, visual, memory, and cold-start gates.
- [ ] Golden PDF is renderer output, not a source-PDF copy.
- [x] Human approver/date: **Project owner — 2026-07-14**
- [ ] Renderer choice: **Deferred to a separate prototype by project-owner decision**
