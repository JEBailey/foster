const esbuild = require("esbuild");
const path = require("node:path");

const production = process.argv.includes("--production");
const watch = process.argv.includes("--watch");

async function main() {
  const context = await esbuild.context({
    absWorkingDir: __dirname,
    entryPoints: [path.join(__dirname, "src", "extension.ts")],
    bundle: true,
    format: "cjs",
    minify: production,
    sourcemap: !production,
    sourcesContent: false,
    platform: "node",
    outfile: path.join(__dirname, "dist", "extension.js"),
    external: ["vscode"],
    logLevel: "info",
  });

  if (watch) {
    await context.watch();
    return;
  }

  await context.rebuild();
  await context.dispose();
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
