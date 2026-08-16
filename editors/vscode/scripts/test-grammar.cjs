const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const grammarPath = path.join(__dirname, "..", "syntaxes", "foster.tmLanguage.json");
const grammar = JSON.parse(fs.readFileSync(grammarPath, "utf8"));
const literals = grammar.repository.strings.patterns;
const codePoint = new RegExp(`^(?:${literals[0].match})$`);
const stringEscape = new RegExp(`^(?:${literals[1].patterns[0].match})$`);

for (const literal of [String.raw`'\\'`, String.raw`'\"'`, String.raw`'\''`, String.raw`'\n'`, `'x'`]) {
  assert.match(literal, codePoint, `code-point grammar must recognize ${literal}`);
}

for (const escape of [String.raw`\\`, String.raw`\"`, String.raw`\n`, String.raw`\r`, String.raw`\t`]) {
  assert.match(escape, stringEscape, `string grammar must recognize ${escape}`);
}

for (const unsupported of [String.raw`\b`, String.raw`\/`, String.raw`\u1234`]) {
  assert.doesNotMatch(
    unsupported,
    stringEscape,
    `grammar must not advertise unsupported Foster escape ${unsupported}`,
  );
}
