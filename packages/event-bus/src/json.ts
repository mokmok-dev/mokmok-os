import { Option } from "effect";

export const parseJson = (text: string): Option.Option<unknown> => {
  try {
    return Option.some(JSON.parse(text));
  } catch {
    return Option.none();
  }
};

export const stringifyJson = (value: unknown): string => JSON.stringify(value);
