const clock = new Intl.DateTimeFormat(undefined, {
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
});

export const clockTime = (millis: number): string => clock.format(millis);

export const isoTime = (millis: number): string => new Date(millis).toISOString();
