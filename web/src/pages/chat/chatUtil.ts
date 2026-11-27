// Коммуникации: общие запросы (список переписок, непрочитанное для меню), черновики поля ввода,
// «печатает…», уведомления на рабочем столе и разбор текста сообщения (ссылки, код, жирный).
import { useQuery } from "@tanstack/react-query";
import { useLocation } from "@tanstack/react-router";
import { useEffect, useRef, useSyncExternalStore } from "react";
import { ApiError, api, ok, type ChatFile, type ChatItem, type ChatQuote } from "../../api/client";
import { csrfToken } from "../../api/client";
import { postFile } from "../helpdesk/helpdeskUtil";
import { useOrg } from "../../session";

/** Мои переписки (`hidden` — скрытые бывшие чаты). Обновляются по событию сервера `chat`. */
export function useChats(hidden = false) {
  return useQuery({
    queryKey: ["chats", hidden],
    queryFn: () => ok<ChatItem[]>(api.GET("/api/chat", { params: { query: { hidden } } })),
    refetchInterval: 120_000,
  });
}

function useMainChats() {
  const on = useOrg().data?.modules.chat === true;
  return useQuery({
    queryKey: ["chats", false],
    queryFn: () => ok<ChatItem[]>(api.GET("/api/chat", { params: { query: { hidden: false } } })),
    enabled: on,
    refetchInterval: 120_000,
  });
}

/** Непрочитанное во всех видимых переписках — для пункта меню. Заблокированные и без звука не считаются. */
export function useChatUnread(): number {
  const q = useMainChats();
  return (q.data ?? []).filter((c) => !c.locked && !c.muted).reduce((n, c) => n + c.unread, 0);
}

/** Адрес вложения переписки. */
export const chatFileUrl = (id: string) => `/api/chat/files/${id}`;

/** Сколько минут после отправки сообщение можно править (как на сервере). */
export const EDIT_MINUTES = 15;

/** Вложений в одном сообщении (как на сервере). */
export const MAX_FILES = 10;

/** Реакции (как на сервере). */
export const REACTIONS = ["👍", "❤️", "😂", "😮", "😢", "🙏", "👎", "🔥", "🎉", "👌", "✅", "🤝"] as const;

// ------------------------------------------------------------------ Вложения поля ввода

/** Вложения поля ввода по чатам. Живут вне компонента: загрузка в фоне доходит до своего чата, даже если
 *  открыли другой чат или правку сообщения, а занятые места считаются вместе с ещё не загруженными. */
export type PendingFiles = { files: ChatFile[]; uploading: number; error: string | null };
const EMPTY: PendingFiles = { files: [], uploading: 0, error: null };
const pending = new Map<string, PendingFiles>();
const listeners = new Set<() => void>();

function change(chat: string, f: (p: PendingFiles) => PendingFiles) {
  const next = f(pending.get(chat) ?? EMPTY);
  if (next.files.length || next.uploading || next.error) pending.set(chat, next);
  else pending.delete(chat);
  for (const l of listeners) l();
}

const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => listeners.delete(l);
};

export function usePendingFiles(chat: string): PendingFiles {
  return useSyncExternalStore(subscribe, () => pending.get(chat) ?? EMPTY);
}

/** Загрузить файлы в черновик чата; лишние сверх {@link MAX_FILES} не загружаются. */
export async function uploadPending(chat: string, list: File[], text: { tooMany: string; failed: string }) {
  const p = pending.get(chat) ?? EMPTY;
  const room = Math.max(MAX_FILES - p.files.length - p.uploading, 0);
  const take = list.slice(0, room);
  change(chat, (q) => ({ ...q, uploading: q.uploading + take.length, error: list.length > room ? text.tooMany : null }));
  await Promise.all(
    take.map(async (f) => {
      try {
        const u = await postFile<ChatFile>(`/api/chat/${chat}/files`, f);
        change(chat, (q) => ({ ...q, files: [...q.files, u], uploading: q.uploading - 1 }));
      } catch (e) {
        const error = e instanceof ApiError ? `${f.name}: ${e.message}` : text.failed;
        change(chat, (q) => ({ ...q, uploading: q.uploading - 1, error }));
      }
    }),
  );
}

/** Убрать вложения из черновика (отправленные или снятые крестиком). */
export function dropPending(chat: string, ids: string[]) {
  change(chat, (q) => ({ ...q, files: q.files.filter((f) => !ids.includes(f.id)), error: null }));
}

// ------------------------------------------------------------------ «Печатает…»

/** Кто печатает в каком чате: событие сервера живёт 6 секунд. */
const typing = new Map<string, Map<string, number>>();
const typingListeners = new Set<() => void>();
const TYPING_TTL = 6000;
let typingSnapshot = new Map<string, string[]>();

function rebuildTyping() {
  const now = Date.now();
  const next = new Map<string, string[]>();
  for (const [chat, users] of typing) {
    for (const [u, at] of users) if (now - at > TYPING_TTL) users.delete(u);
    if (users.size > 0) next.set(chat, [...users.keys()]);
    else typing.delete(chat);
  }
  typingSnapshot = next;
  for (const l of typingListeners) l();
}

/** Событие сервера `typing`: собеседник набирает текст. */
export function markTyping(chat: string, user: string) {
  const users = typing.get(chat) ?? new Map<string, number>();
  users.set(user, Date.now());
  typing.set(chat, users);
  rebuildTyping();
  setTimeout(rebuildTyping, TYPING_TTL + 100);
}

/** Сообщение пришло — набор в этом чате закончен (у события нет автора — сбрасываются все). */
export function clearTyping(chat: string, user?: string) {
  const users = typing.get(chat);
  if (!users) return;
  if (user) users.delete(user);
  else users.clear();
  rebuildTyping();
}

const NOBODY: string[] = [];
export function useTyping(chat: string): string[] {
  return useSyncExternalStore(
    (l) => {
      typingListeners.add(l);
      return () => typingListeners.delete(l);
    },
    () => typingSnapshot.get(chat) ?? NOBODY,
  );
}

const lastTyping = new Map<string, number>();
/** Сообщить «печатаю» — не чаще раза в 3 секунды на чат. Ошибки не важны. */
export function sendTyping(chat: string) {
  const now = Date.now();
  if (now - (lastTyping.get(chat) ?? 0) < 3000) return;
  lastTyping.set(chat, now);
  const headers: Record<string, string> = {};
  const t = csrfToken();
  if (t) headers["X-CSRF-Token"] = t;
  void fetch(`/api/chat/${chat}/typing`, { method: "POST", headers, credentials: "same-origin" }).catch(() => undefined);
}

// ------------------------------------------------------------------ Уведомления

const ALERTS_KEY = "pult.chat.alerts";
export type AlertPrefs = { desktop: boolean; sound: boolean };

export function alertPrefs(): AlertPrefs {
  try {
    const v = JSON.parse(localStorage.getItem(ALERTS_KEY) ?? "{}") as Partial<AlertPrefs>;
    return { desktop: v.desktop === true, sound: v.sound !== false };
  } catch {
    return { desktop: false, sound: true };
  }
}

export function saveAlertPrefs(p: AlertPrefs) {
  try {
    localStorage.setItem(ALERTS_KEY, JSON.stringify(p));
  } catch {
    // Хранилище браузера недоступно — настройка действует до перезагрузки.
  }
}

/** Короткий сигнал без файла: звук генерируется в браузере. Контекст один на вкладку. */
let audio: AudioContext | null = null;
function beep() {
  try {
    audio ??= new window.AudioContext();
    const ctx = audio;
    if (ctx.state === "suspended") void ctx.resume();
    if (ctx.state !== "running") return;
    const o = ctx.createOscillator();
    const g = ctx.createGain();
    o.type = "sine";
    o.frequency.setValueAtTime(880, ctx.currentTime);
    o.frequency.exponentialRampToValueAtTime(660, ctx.currentTime + 0.12);
    g.gain.setValueAtTime(0.0001, ctx.currentTime);
    g.gain.exponentialRampToValueAtTime(0.12, ctx.currentTime + 0.02);
    g.gain.exponentialRampToValueAtTime(0.0001, ctx.currentTime + 0.25);
    o.connect(g).connect(ctx.destination);
    o.start();
    o.stop(ctx.currentTime + 0.26);
  } catch {
    // Звук недоступен (политика браузера) — ничего страшного.
  }
}

/** Новое сообщение в чате, который сейчас не открыт (или вкладка свёрнута): звук и уведомление
 *  на рабочем столе (если включены). Чаты без звука молчат. Текст берётся из списка переписок —
 *  его сервер отдаёт только участнику. */
export function useChatAlerts(onOpen: (chat: string) => void) {
  const q = useMainChats();
  const path = useLocation().pathname;
  const prev = useRef<Map<string, number> | null>(null);
  useEffect(() => {
    const list = q.data;
    if (!list) return;
    const before = prev.current;
    prev.current = new Map(list.map((c) => [c.id, c.unread]));
    if (!before) return;
    const prefs = alertPrefs();
    const fresh = list.filter((c) => !c.muted && !c.locked && c.unread > (before.get(c.id) ?? 0));
    const visible = (c: ChatItem) => document.visibilityState === "visible" && path === `/chats/${c.id}`;
    const alerts = fresh.filter((c) => !visible(c));
    if (alerts.length === 0) return;
    if (prefs.sound) beep();
    if (prefs.desktop && "Notification" in window && Notification.permission === "granted" && document.visibilityState !== "visible") {
      for (const c of alerts.slice(0, 3)) {
        const n = new Notification(c.title, { body: c.preview ?? "📎", tag: `chat-${c.id}` });
        n.onclick = () => {
          window.focus();
          onOpen(c.id);
          n.close();
        };
      }
    }
  }, [q.data, path, onOpen]);
}

// ------------------------------------------------------------------ Текст сообщения

export type Token =
  | { kind: "text"; text: string }
  | { kind: "link"; text: string; href: string }
  | { kind: "code"; text: string }
  | { kind: "pre"; text: string }
  | { kind: "bold"; text: string }
  | { kind: "italic"; text: string }
  | { kind: "strike"; text: string };

const URL_RE = /\bhttps?:\/\/[^\s<>"]+[^\s<>".,;:!?)\]}'»]/giu;

function linkify(text: string, out: Token[]) {
  let last = 0;
  for (const m of text.matchAll(URL_RE)) {
    const i = m.index ?? 0;
    if (i > last) out.push({ kind: "text", text: text.slice(last, i) });
    out.push({ kind: "link", text: m[0], href: m[0] });
    last = i + m[0].length;
  }
  if (last < text.length) out.push({ kind: "text", text: text.slice(last) });
}

const INLINE_RE = /`([^`\n]+)`|\*\*([^*\n]+)\*\*|__([^_\n]+)__|~~([^~\n]+)~~|(?<![\p{L}\p{N}])_([^_\n]+)_(?![\p{L}\p{N}])/gu;

function inline(text: string, out: Token[]) {
  let last = 0;
  for (const m of text.matchAll(INLINE_RE)) {
    const i = m.index ?? 0;
    if (i > last) linkify(text.slice(last, i), out);
    if (m[1] !== undefined) out.push({ kind: "code", text: m[1] });
    else if (m[2] !== undefined || m[3] !== undefined) out.push({ kind: "bold", text: m[2] ?? m[3] ?? "" });
    else if (m[4] !== undefined) out.push({ kind: "strike", text: m[4] });
    else out.push({ kind: "italic", text: m[5] ?? "" });
    last = i + m[0].length;
  }
  if (last < text.length) linkify(text.slice(last), out);
}

/** Разметка как в мессенджерах: ```блок кода```, `код`, **жирный**, _курсив_, ~~зачёркнутый~~ и ссылки. */
export function tokenize(body: string): Token[] {
  const out: Token[] = [];
  const parts = body.split(/```/);
  parts.forEach((p, i) => {
    // Нечётные части — внутри ```…```; незакрытый блок остаётся текстом.
    if (i % 2 === 1 && i < parts.length - 1) out.push({ kind: "pre", text: p.replace(/^\n/, "").replace(/\n$/, "") });
    else inline(i % 2 === 1 ? "```" + p : p, out);
  });
  return out;
}

/** Сообщение только из эмодзи (до трёх): показывается крупно, без пузыря. */
export function onlyEmoji(body: string): boolean {
  const s = body.trim();
  if (!s || s.length > 24) return false;
  return /^(?:\p{Extended_Pictographic}(?:️|‍\p{Extended_Pictographic}|[\u{1F3FB}-\u{1F3FF}])*\s*){1,3}$/u.test(s);
}

/** Ключ дня для разделителей ленты. */
export const dayKey = (iso: string) => new Date(iso).toDateString();

/** Цитата из своего же сообщения (черновик ответа): первая строка, как её покажет сервер. */
export function quoteOf(m: { id: string; author_name: string; body: string; files: unknown[] }): ChatQuote {
  const line = m.body.split("\n").find((l) => l.trim()) ?? "";
  return { id: m.id, author_name: m.author_name, preview: [...line].slice(0, 100).join(""), files: m.files.length };
}
