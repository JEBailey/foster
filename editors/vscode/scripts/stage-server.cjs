const fs = require("node:fs");
const path = require("node:path");

const extensionRoot = path.resolve(__dirname, "..");
const repositoryRoot = path.resolve(extensionRoot, "..", "..");
const executable = process.platform === "win32" ? "foster.exe" : "foster";
const configuredServer = process.env.FOSTER_SERVER_PATH?.trim();
const sourceServer = configuredServer
  ? path.resolve(configuredServer)
  : path.join(repositoryRoot, "target", "release", executable);

if (!fs.statSync(sourceServer, { throwIfNoEntry: false })?.isFile()) {
  throw new Error(
    `Foster release compiler not found at ${sourceServer}. ` +
      "Run `cargo build --release --locked` or set FOSTER_SERVER_PATH.",
  );
}

const serverDirectory = path.join(extensionRoot, "server");
fs.rmSync(serverDirectory, { recursive: true, force: true });
fs.mkdirSync(serverDirectory, { recursive: true });
const stagedServer = path.join(serverDirectory, executable);
fs.copyFileSync(sourceServer, stagedServer);
if (process.platform !== "win32") {
  fs.chmodSync(stagedServer, 0o755);
}

const sourceLibrary = path.join(repositoryRoot, "library");
const stagedLibrary = path.join(extensionRoot, "library");
fs.rmSync(stagedLibrary, { recursive: true, force: true });
fs.cpSync(sourceLibrary, stagedLibrary, { recursive: true });

for (const license of ["LICENSE-MIT", "LICENSE-APACHE"]) {
  fs.copyFileSync(
    path.join(repositoryRoot, license),
    path.join(extensionRoot, license),
  );
}

const combinedLicense = [
  "Foster is available under either the MIT License or the Apache License, Version 2.0, at your option.",
  "",
  fs.readFileSync(path.join(repositoryRoot, "LICENSE-MIT"), "utf8"),
  "",
  fs.readFileSync(path.join(repositoryRoot, "LICENSE-APACHE"), "utf8"),
].join("\n");
fs.writeFileSync(path.join(extensionRoot, "LICENSE"), combinedLicense);

console.log(`Staged ${sourceServer} as ${stagedServer}`);
