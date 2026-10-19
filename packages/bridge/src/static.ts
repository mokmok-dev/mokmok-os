import { readFile, stat } from "node:fs/promises";
import type { ServerResponse } from "node:http";
import { extname, resolve, sep } from "node:path";

const CONTENT_TYPES: Readonly<Record<string, string>> = {
  ".css": "text/css; charset=utf-8",
  ".gif": "image/gif",
  ".html": "text/html; charset=utf-8",
  ".ico": "image/x-icon",
  ".jpeg": "image/jpeg",
  ".jpg": "image/jpeg",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".map": "application/json; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".ttf": "font/ttf",
  ".txt": "text/plain; charset=utf-8",
  ".wasm": "application/wasm",
  ".webp": "image/webp",
  ".woff": "font/woff",
  ".woff2": "font/woff2",
};

interface Asset {
  readonly body: Buffer;
  readonly contentType: string;
}

const respond = (
  response: ServerResponse,
  method: string,
  status: number,
  body: Buffer | string,
  contentType: string,
  extraHeaders: Readonly<Record<string, string>> = {},
): void => {
  const buffer = typeof body === "string" ? Buffer.from(body) : body;
  response.writeHead(status, {
    "content-type": contentType,
    "content-length": buffer.byteLength,
    ...extraHeaders,
  });
  response.end(method === "HEAD" ? undefined : buffer);
};

export const respondText = (
  response: ServerResponse,
  method: string,
  status: number,
  text: string,
): void => {
  respond(response, method, status, text, "text/plain; charset=utf-8", {
    "cache-control": "no-store",
  });
};

const readAsset = async (file: string): Promise<Asset | null> => {
  try {
    const body = await readFile(file);
    const contentType = CONTENT_TYPES[extname(file).toLowerCase()];

    return { body, contentType: contentType ?? "application/octet-stream" };
  } catch {
    return null;
  }
};

const directoryExists = async (directory: string): Promise<boolean> => {
  try {
    return (await stat(directory)).isDirectory();
  } catch {
    return false;
  }
};

export const serveStatic = async (
  method: string,
  response: ServerResponse,
  rawUrl: string,
  staticDir: string | undefined,
): Promise<void> => {
  if (staticDir === undefined) {
    respondText(response, method, 404, "static assets are not configured");
    return;
  }

  // Inspect the raw request target: WHATWG URL normalizes %2e%2e before the
  // handler sees a pathname, so encoded traversal must be rejected here.
  const rawPath = rawUrl.startsWith("/")
    ? (rawUrl.split("?")[0] ?? "/")
    : new URL(rawUrl, "http://localhost").pathname;

  let decoded: string;
  try {
    decoded = decodeURIComponent(rawPath);
  } catch {
    respondText(response, method, 400, "malformed request path");
    return;
  }

  if (decoded.includes("\0")) {
    respondText(response, method, 400, "malformed request path");
    return;
  }

  if (decoded.split("/").includes("..")) {
    respondText(response, method, 403, "forbidden");
    return;
  }

  const root = resolve(staticDir);
  const candidate = resolve(root, `.${decoded}`);
  if (candidate !== root && !candidate.startsWith(`${root}${sep}`)) {
    respondText(response, method, 403, "forbidden");
    return;
  }

  const file =
    decoded.endsWith("/") || decoded === ""
      ? resolve(candidate, "index.html")
      : candidate;

  const asset = await readAsset(file);
  if (asset !== null) {
    respond(response, method, 200, asset.body, asset.contentType, {
      "cache-control": "no-store",
    });
    return;
  }

  if (extname(decoded) === "") {
    const index = await readAsset(resolve(root, "index.html"));
    if (index !== null) {
      respond(response, method, 200, index.body, index.contentType, {
        "cache-control": "no-store",
      });
      return;
    }
  }

  if (!(await directoryExists(root))) {
    respondText(
      response,
      method,
      404,
      `console static directory not found: ${root}`,
    );
    return;
  }

  respondText(response, method, 404, "not found");
};
