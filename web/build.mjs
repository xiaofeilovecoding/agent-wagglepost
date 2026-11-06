import * as esbuild from "esbuild";
import { mkdirSync, copyFileSync } from "node:fs";

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
console.log("built web/dist");
