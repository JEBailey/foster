# Unicode text in Foster

Unicode assigns numbers to text characters and supplies rules and properties for
working with them. Foster's Unicode data is a set of lookup tables used by its
text algorithms. It is not a font, dictionary, translation service, or collection
of pictures. Most programs use `String` and `CodePoint`, not the raw tables.

## Why one character can have several lengths

| Term | Simple meaning | Foster API |
| --- | --- | --- |
| Code point | A numbered position in Unicode, conventionally written in hexadecimal such as `U+0041` for A | `CodePoint.as_int()` returns its number |
| Scalar | A code point excluding the range reserved for UTF-16 surrogates | One `CodePoint` value |
| Grapheme cluster | A group that often looks like one character, such as a letter plus its accent | `String.length`, `String.slice()` |
| UTF-8 byte | A unit of encoded storage; a scalar takes one to four bytes | `String.byte_length()` |

These distinctions follow the [Unicode glossary](https://www.unicode.org/glossary/).
A cluster is not a promise about font width or how many glyphs will be drawn.

This example deliberately spells the accented e using an e followed by a combining
accent. Save it as `unicode-example.fos` and run `foster run unicode-example.fos`:

```foster
import core.string

func main() -> Int {
    let text = "é"
    assert(text.length == 1)
    assert(text.scalar_length() == 2)
    assert(text.byte_length() == 3)
    assert(text.slice(0, 1) == text)
    assert("Straße".upper() == "STRASSE")
    assert("Straße".case_fold() == "strasse")
    0
}
```

Use grapheme operations when splitting text into whole text elements. Use
`code_points()` or `StringCursor` to inspect individual scalars. Use bytes for
encoded storage or binary protocols. Do not pass a byte offset to a method that
expects a grapheme index.

## What the tables mean

- **Categories** are broad labels such as letter, number, punctuation, or mark.
  `CodePoint.category()` returns a named `GeneralCategory` value.
- **Properties** are additional facts such as alphabetic or whitespace status.
  Classification methods answer these questions without exposing packed numbers.
- **Simple casing** maps one code point to one code point. **Full casing** can
  expand to text: uppercasing sharp s gives `SS` in the example above.
- **Case folding** gives case variants a common form for comparisons. It is not
  intended to choose attractive capitalization for display.
- **Grapheme-break properties** help decide where one text element ends.
  **Extended pictographic** data supports emoji joining rules; it is not a complete
  emoji detector. **Indic conjunct** data helps keep certain connected letter
  sequences together. Foster combines these properties using the
  [Unicode segmentation rules](https://www.unicode.org/reports/tr29/tr29-47.html).

## What does not happen automatically

Normalization converts equivalent spellings to a common representation. For
example, precomposed é and an e followed by an accent use different scalar
sequences. Case conversion and folding in Foster do not normalize them, so do
not assume folding makes every visually identical spelling equal.

Locale tailoring adjusts rules for a language or region. Foster uses default
Unicode casing rather than language-specific rules. Unicode 17.0.0 is pinned:
Foster uses the same data version in its VM and native backend.

The packed hexadecimal strings in `core.unicode.data` are an internal storage
format. Public visibility allows library modules to share them; it does not make
their numeric IDs a convenient application API. Maintainers can read the
[generator guide](../tools/unicode/README.md) for record formats and regeneration.
