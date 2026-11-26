// Коммуникации: общие запросы (список переписок, непрочитанное для меню) и черновики поля ввода.
import { useQuery } from "@tanstack/react-query";
import { useSyncExternalStore } from "react";
import { ApiError, api, ok, type ChatFile, type ChatItem } from "../../api/client";
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

/** Непрочитанное во всех видимых переписках — для пункта меню. Заблокированные чаты не считаются. */
export function useChatUnread(): number {
  const on = useOrg().data?.modules.chat === true;
  const q = useQuery({
    queryKey: ["chats", false],
    queryFn: () => ok<ChatItem[]>(api.GET("/api/chat", { params: { query: { hidden: false } } })),
    enabled: on,
    refetchInterval: 120_000,
  });
  return (q.data ?? []).filter((c) => !c.locked).reduce((n, c) => n + c.unread, 0);
}

/** Сколько минут после отправки сообщение можно править (как на сервере). */
export const EDIT_MINUTES = 15;

/** Вложений в одном сообщении (как на сервере). */
export const MAX_FILES = 10;

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
