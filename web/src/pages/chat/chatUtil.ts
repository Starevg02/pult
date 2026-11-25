// Коммуникации: общие запросы (список переписок, непрочитанное для меню).
import { useQuery } from "@tanstack/react-query";
import { api, ok, type ChatItem } from "../../api/client";
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
