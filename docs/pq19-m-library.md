# M library: Portuguese date names and Record.ToTable

> **Last reviewed:** 2026-10-17 | **Last updated:** 2026-10-17 | **Change:** minor (two bounded M-library additions) | **Target:** Microsoft Excel (Microsoft 365)

This follow-up extends the M interpreter represented by public commit
20f2e36bc4c5dc01ee532aecad5ee86a022329c4. It adds no dependency and changes neither
the query model nor editor, source permissions, worksheet-error projection or storage.

## Portuguese names and standard formats

Full day/month names are available for pt, pt-BR and pt-PT through the existing culture
normalization, including case and underscore variants. For example:

    Date.DayOfWeekName(#date(2026,5,21), "pt-BR")  // quinta-feira

The shared formatter uses dd/MM/yyyy, dddd, d 'de' MMMM 'de' yyyy, HH:mm and HH:mm:ss
for the Portuguese standard profile. Date.ToText, DateTime.ToText, Time.ToText and
the existing corresponding Text.From paths share those names/patterns. Default culture,
numeric conversion and other languages are unchanged.

This is a bounded full-name/standard-pattern profile. Existing three-character MMM/ddd
abbreviations, textual-month parsing and FromText format-option limits remain. Complete
regional NLS/ICU or native Excel parity is not claimed.

## Record.ToTable

Record.ToTable(record) returns Name and Value columns, one row per field in input order.
An empty record keeps both columns. Values retain their existing M kinds, held errors
and lazy objects without being forced by the conversion. Null may be a field value;
null and other non-record arguments are rejected as the record argument.

The handler checks the row limit and internal record shape before output allocation
and charges the evaluator's field-copy budget. Internal column metadata stays Any/Any.
Value.Type keeps this base's existing primitive-table behavior; this addition does not
add table-field type reflection or general record-field laziness. Projection to worksheet
cells continues to follow the existing base, including its error behavior.

## Sources, attribution and verification

Behavior follows Microsoft's public [Record.ToTable](https://learn.microsoft.com/en-us/powerquery-m/record-totable),
[record values](https://learn.microsoft.com/en-us/powerquery-m/m-spec-values#record),
[Date.DayOfWeekName](https://learn.microsoft.com/en-us/powerquery-m/date-dayofweekname),
[Date.MonthName](https://learn.microsoft.com/en-us/powerquery-m/date-monthname) and
[Date.ToText](https://learn.microsoft.com/en-us/powerquery-m/date-totext) contracts.

This delta is derived from the Herr-Enklo-authored PQ19 change and adapted to the public
M interpreter. It builds on #1203 by Herr-Enklo and #1532 by Anton Belkin. A separate
Gab/PQ18 comparison identified the gaps; no GabLavareda source code or third-party data
file is copied. Existing project licenses and prior authorship are preserved.

Six Portuguese tests cover all full names, formatting, date/time/null/error cases and
existing culture fallbacks. Nine Record tests cover field order, the empty schema,
nested values, argument validation, held errors, existing lazy objects and resource/shape
limits. Two tests for later local error diagnostics/editor preview and a Value.Type
field-reflection assertion are deliberately outside this base. Validation results for
these 15 cases and this exact branch belong in the pull request; earlier tests of a
different local integration tree do not establish the results of this port.
