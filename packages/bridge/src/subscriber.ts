import { randomUUID } from "node:crypto";
import { mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { dirname } from "node:path";

const UUID_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

const readExisting = async (file: string): Promise<string | null> => {
  try {
    const text = await readFile(file, "utf8");
    const candidate = text.trim();

    return UUID_PATTERN.test(candidate) ? candidate : null;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return null;
    throw error;
  }
};

/** A subscriber id written once is what lets a restarted bridge resume the bus checkpoint. */
export const loadSubscriberId = async (file: string): Promise<string> => {
  const existing = await readExisting(file);
  if (existing !== null) return existing;

  const id = randomUUID();
  await mkdir(dirname(file), { recursive: true });

  // A rename makes a concurrent reader see either the old id or the new one, never a torn file.
  const temporary = `${file}.${process.pid}.${randomUUID()}.tmp`;
  try {
    await writeFile(temporary, `${id}\n`, { encoding: "utf8", mode: 0o600 });
    await rename(temporary, file);
  } catch (error) {
    await rm(temporary, { force: true });
    throw error;
  }

  return id;
};
