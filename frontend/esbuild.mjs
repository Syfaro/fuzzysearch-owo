import * as esbuild from "esbuild";
import { sassPlugin } from "esbuild-sass-plugin";
import { sentryEsbuildPlugin } from "@sentry/esbuild-plugin";
import copy from "esbuild-plugin-copy";
import "dotenv/config";

const IS_PROD = process.env.NODE_ENVIRONMENT === "production";

const define = {
  "process.env.SENTRY_DSN": JSON.stringify(process.env["SENTRY_DSN"]),
};

function buildOpts(entryPoints) {
  return {
    entryPoints,
    bundle: true,
    outdir: "./dist/",
    minify: IS_PROD,
    sourcemap: true,
    drop: IS_PROD ? ["console"] : [],
    target: ["es2020"],
    plugins: [
      sassPlugin(),
      copy({
        assets: {
          from: ["./assets/img/*"],
          to: ["./img"],
        },
        watch: true,
      }),
      sentryEsbuildPlugin({
        telemetry: false,
      }),
    ],
    loader: {
      ".woff": "file",
      ".woff2": "file",
    },
    define,
  };
}

await Promise.all([
  esbuild.build(buildOpts(["src/entrypoints/main.ts"])),
  esbuild.build(buildOpts(["src/entrypoints/admin.ts"])),
]);
