import type { Person } from "../api/client";
import { prefs } from "../prefs";

export const COLUMNS = ["position", "department", "work", "mobile", "email", "site", "room"] as const;
export type Column = (typeof COLUMNS)[number];
export const DEFAULT_COLUMNS: Column[] = ["position", "department", "work", "mobile", "site"];

/** ФИО в порядке из личной настройки: «Иванов Иван Иванович» или «Иван Иванович Иванов». */
export function fullName(p: Pick<Person, "last_name" | "first_name" | "middle_name">): string {
  const parts = prefs().name_order === "first_last" ? [p.first_name, p.middle_name, p.last_name] : [p.last_name, p.first_name, p.middle_name];
  return parts.filter(Boolean).join(" ");
}

/** Ссылка «Написать» по шаблону администратора; адрес экранируется. */
export function mailHref(template: string, email: string): string {
  const href = template.split("{email}").join(encodeURIComponent(email));
  // Шаблон задаёт администратор; всё равно только почта или веб — не `javascript:`.
  return /^(mailto:|https?:)/i.test(href) ? href : `mailto:${encodeURIComponent(email)}`;
}
