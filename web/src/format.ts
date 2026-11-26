// Форматирование для показа: инициалы, даты — по языку интерфейса и личной настройке формата.
import { intlTag } from "./i18n";
import type { UserPrefs } from "./api/client";
import { prefs } from "./prefs";

export function initials(name: string): string {
  // Только слова с буквой или цифрой: «АДМ - Статиков» → «АС», а не «А-».
  const parts = name.trim().split(/\s+/).filter((p) => /[\p{L}\p{N}]/u.test(p));
  return (parts[0]?.[0] ?? "") + (parts[1]?.[0] ?? "");
}

const pad = (n: number) => String(n).padStart(2, "0");

/** Дата и время: «как в языке» или выбранный формат (31.12.2026, 2026-12-31, 12/31/2026), 24 или 12 часов. */
export function formatDate(iso: string | null | undefined, lang: string, p: Pick<UserPrefs, "date_format" | "clock"> = prefs()): string {
  if (!iso) return "—";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "—";
  const hour12 = p.clock === "h12";
  if (p.date_format === "auto") {
    return d.toLocaleString(intlTag(lang), { dateStyle: "short", timeStyle: "short", hour12 });
  }
  const [y, m, day] = [d.getFullYear(), pad(d.getMonth() + 1), pad(d.getDate())];
  const date = p.date_format === "ymd" ? `${y}-${m}-${day}` : p.date_format === "mdy" ? `${m}/${day}/${y}` : `${day}.${m}.${y}`;
  const time = d.toLocaleTimeString(intlTag(lang), { hour: "2-digit", minute: "2-digit", hour12 });
  return `${date} ${time}`;
}
