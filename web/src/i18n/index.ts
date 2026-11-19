// Языки: русский по умолчанию, английский, китайский; все строки интерфейса — через ключи.
// Выбор языка: явный выбор в этом браузере (localStorage) > язык учётки (`me.locale`) > язык браузера (до входа) > русский.
// Сервер узнаёт язык из cookie `pult_lang` — на нём приходят ошибки, подписи отчётов и выгрузки.
import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import en from "./en.json";
import ru from "./ru.json";
import zh from "./zh.json";

export type Lang = "ru" | "en" | "zh";

/** Языки и их названия — на самих языках, их не переводят. */
export const LANGUAGES: readonly (readonly [Lang, string])[] = [
  ["ru", "Русский"],
  ["en", "English"],
  ["zh", "中文"],
];

/** Код языка из строки (учётка, localStorage); неизвестное — `null`. */
export function asLang(v: string | null | undefined): Lang | null {
  return LANGUAGES.some(([l]) => l === v) ? (v as Lang) : null;
}

/** Тег для дат и чисел. */
export function intlTag(lang: string): string {
  return lang === "en" ? "en-GB" : lang === "zh" ? "zh-CN" : "ru-RU";
}

export const resources = { ru: { translation: ru }, en: { translation: en }, zh: { translation: zh } } as const;

/** Язык, явно выбранный в этом браузере (или `null`). */
export function savedLanguage(): Lang | null {
  try {
    return asLang(localStorage.getItem("pult.lang"));
  } catch {
    return null;
  }
}

function apply(lng: Lang) {
  document.documentElement.lang = lng;
  const secure = location.protocol === "https:" ? "; Secure" : "";
  document.cookie = `pult_lang=${lng}; Path=/; Max-Age=31536000; SameSite=Lax${secure}`;
}

/** Первый подходящий язык браузера (`zh-CN` → `zh`) или `null`. */
export function browserLanguage(): Lang | null {
  const list = typeof navigator === "undefined" ? [] : (navigator.languages ?? [navigator.language]);
  for (const tag of list) {
    const l = asLang(tag?.toLowerCase().split("-")[0]);
    if (l) return l;
  }
  return null;
}

const initial: Lang = savedLanguage() ?? browserLanguage() ?? "ru";
apply(initial);

void i18n.use(initReactI18next).init({
  resources,
  lng: initial,
  fallbackLng: "ru",
  interpolation: { escapeValue: false }, // React сам экранирует вывод
});

/** Сменить язык. `remember` — запомнить как явный выбор этого браузера. */
export function setLanguage(lng: Lang, remember = true) {
  apply(lng);
  void i18n.changeLanguage(lng);
  if (!remember) return;
  try {
    localStorage.setItem("pult.lang", lng);
  } catch {
    // приватный режим — язык просто не запомнится
  }
}

export default i18n;
