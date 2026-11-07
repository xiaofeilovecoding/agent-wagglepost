import * as esbuild from "esbuild";
import { mkdirSync, copyFileSync, readFileSync, writeFileSync } from "node:fs";
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
// WP12: record asset digests so the embedded bundle is independently checkable
const lines = ["app.js", "styles.css", "index.html"]
  .map((f) => {
    const h = createHash("sha256").update(readFileSync(`dist/${f}`)).digest("hex");
    return `${h}  ${f}`;
  })
  .join("\n");
writeFileSync("dist/ASSETS.sha", lines + "\n");
console.log("built web/dist (ASSETS.sha written)");
