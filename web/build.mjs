import * as esbuild from "esbuild";
import { mkdirSync, copyFileSync, readFileSync, writeFileSync, readdirSync, statSync } from "node:fs";
import { createHash } from "node:crypto";

mkdirSync("dist", { recursive: true });
await esbuild.build({
  entryPoints: ["src/app.tsx"],
  bundle: true,
  minify: true,
  sourcemap: false,
  outfile: "dist/app.js",
  jsx: "automatic",
  define: { "process.env.NODE_ENV": '"production"' },
  logLevel: "info",
});
copyFileSync("src/index.html", "dist/index.html");
copyFileSync("src/styles.css", "dist/styles.css");

// WP12: the manifest records content digests for BOTH sides of the build —
//   in  <sha256>  <path relative to web/>    every real build input
//   out <sha256>  <path relative to dist/>   every produced artifact
// build.rs verifies both sets plus their completeness, so a bundle built from
// older sources or carrying a swapped/extra artifact fails `cargo build`.
function sha256(p) {
  return createHash("sha256").update(readFileSync(p)).digest("hex");
}
function listSources(dir, prefix = "") {
  const out = [];
  for (const name of readdirSync(dir).sort()) {
    const rel = prefix ? `${prefix}/${name}` : name;
    if (statSync(`${dir}/${name}`).isDirectory()) out.push(...listSources(`${dir}/${name}`, rel));
    else out.push(rel);
  }
  return out;
}
const inputs = [
  ...listSources("src").map((p) => `src/${p}`),
  "build.mjs",
  "package.json",
  "package-lock.json",
].sort();
const outputs = ["app.js", "styles.css", "index.html"];
const lines = [
  "# wagglepost asset manifest v2 — \"in\" paths are relative to web/, \"out\" to web/dist; verified by build.rs",
  ...inputs.map((f) => `in  ${sha256(f)}  ${f.replace(/\\/g, "/")}`),
  ...outputs.map((f) => `out ${sha256(`dist/${f}`)}  ${f}`),
];
writeFileSync("dist/ASSETS.sha", lines.join("\n") + "\n");
console.log("built web/dist (ASSETS.sha v2: inputs + outputs)");
