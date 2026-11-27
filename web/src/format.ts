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

/** Только время: «14:05» (или 12-часовое по настройке). */
export function formatTime(iso: string, lang: string, p: Pick<UserPrefs, "clock"> = prefs()): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return d.toLocaleTimeString(intlTag(lang), { hour: "2-digit", minute: "2-digit", hour12: p.clock === "h12" });
}

/** День словами: «12 ноября 2026», в этом году — «12 ноября». */
export function formatDay(iso: string, lang: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "—";
  const thisYear = d.getFullYear() === new Date().getFullYear();
  return d.toLocaleDateString(intlTag(lang), { day: "numeric", month: "long", ...(thisYear ? {} : { year: "numeric" }) });
}

/** Метка в списке переписок: сегодня — время, на этой неделе — день недели, раньше — дата. */
export function formatShort(iso: string, lang: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const now = new Date();
  if (d.toDateString() === now.toDateString()) return formatTime(iso, lang);
  const days = (now.getTime() - d.getTime()) / 86_400_000;
  if (days < 6) return d.toLocaleDateString(intlTag(lang), { weekday: "short" });
  return d.toLocaleDateString(intlTag(lang), { day: "numeric", month: "short", ...(d.getFullYear() === now.getFullYear() ? {} : { year: "2-digit" }) });
}
