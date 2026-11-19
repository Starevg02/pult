// Личные предпочтения интерфейса (как в GLPI): тема, плотность, формат даты и времени, порядок ФИО,
// строк в списках, стартовая страница. Хранятся в учётке (`/api/me/prefs`); тема и плотность ещё и в
// браузере — чтобы страница сразу открывалась в своей теме, до ответа сервера.
import { useMutation, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { api, ok, type Me, type UserPrefs } from "./api/client";
import { setLanguage, type Lang } from "./i18n";

export type ThemeChoice = UserPrefs["theme"];
export type StartPage = UserPrefs["start_page"];

export const DEFAULT_PREFS: UserPrefs = {
  theme: "system",
  density: "comfortable",
  date_format: "auto",
  clock: "h24",
  name_order: "last_first",
  page_size: 50,
  start_page: "home",
};

export const PAGE_SIZES = [25, 50, 100, 200] as const;

export const START_PATHS: Record<StartPage, string> = {
  home: "/",
  tickets: "/tickets",
  desk: "/desk",
  phonebook: "/phonebook",
  kb: "/kb",
  notifications: "/notifications",
};

const THEME_KEY = "pult.theme";
const DENSITY_KEY = "pult.density";

function read(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function write(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Приватное окно или запрет хранилища: тема всё равно применится из учётки после входа.
  }
}

let current: UserPrefs = {
  ...DEFAULT_PREFS,
  theme: asTheme(read(THEME_KEY)),
  density: read(DENSITY_KEY) === "compact" ? "compact" : "comfortable",
};

function asTheme(v: string | null): ThemeChoice {
  return v === "light" || v === "dark" ? v : "system";
}

/** Предпочтения для форматирования вне React (даты, ФИО). */
export function prefs(): UserPrefs {
  return current;
}

/** Применить к странице: `data-theme` задаёт `color-scheme` (токены в app.css — на `light-dark()`). */
export function applyPrefs(p: UserPrefs) {
  current = p;
  const html = document.documentElement;
  if (p.theme === "system") delete html.dataset.theme;
  else html.dataset.theme = p.theme;
  html.dataset.density = p.density;
  write(THEME_KEY, p.theme);
  write(DENSITY_KEY, p.density);
}

/** Тема, которую сейчас видно (для «системной» — по настройке ОС). */
export function shownDark(theme: ThemeChoice = current.theme): boolean {
  if (theme !== "system") return theme === "dark";
  return window.matchMedia?.("(prefers-color-scheme: dark)").matches ?? false;
}

/** Применить сохранённое в браузере до первого кадра (вызывается из main.tsx). */
export function applySaved() {
  applyPrefs(current);
}

const PREFS_KEY = ["prefs"];

/** Сохранить предпочтения: страница меняется сразу, учётка — следом; при ошибке — откат. */
export function useSavePrefs() {
  const qc = useQueryClient();
  return useMutation({
    mutationKey: PREFS_KEY,
    // Сохранения идут по одному: ответ на старое не перезапишет более позднее.
    scope: { id: "prefs" },
    mutationFn: (p: UserPrefs) => ok<UserPrefs>(api.PUT("/api/me/prefs", { body: p })),
    onMutate: (p) => {
      // Перезапрос /api/me в полёте вернул бы прежнюю тему поверх выбранной.
      void qc.cancelQueries({ queryKey: ["me"] });
      const before = current;
      applyPrefs(p);
      qc.setQueryData<Me>(["me"], (me) => (me ? { ...me, prefs: p } : me));
      return { before };
    },
    onError: (_e, _p, ctx) => {
      // Откат — только если за этим сохранением нет следующего.
      if (!ctx || qc.isMutating({ mutationKey: PREFS_KEY }) > 1) return;
      applyPrefs(ctx.before);
      qc.setQueryData<Me>(["me"], (me) => (me ? { ...me, prefs: ctx.before } : me));
    },
    onSuccess: (saved) => {
      if (qc.isMutating({ mutationKey: PREFS_KEY }) <= 1) qc.setQueryData<Me>(["me"], (me) => (me ? { ...me, prefs: saved } : me));
      void qc.invalidateQueries({ queryKey: ["profile"] });
    },
  });
}

export const THEMES: ThemeChoice[] = ["light", "dark", "system"];

/** Сменить язык: в браузере сразу, в учётке — для писем; тексты сервера перезапрашиваются на новом языке.
 *  Если сохранить в учётке не удалось (нет сети) — сохранится при следующей смене. */
export function changeLanguage(lng: Lang, qc: QueryClient) {
  setLanguage(lng);
  void api
    .PUT("/api/me/locale", { body: { locale: lng } })
    .then(() => qc.invalidateQueries())
    .catch(() => undefined);
}
