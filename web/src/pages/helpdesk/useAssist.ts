// Включён ли помощник: от этого зависят его раздел и кнопки.
import { useQuery } from "@tanstack/react-query";
import { api, ok, type AssistStatus } from "../../api/client";
import { can, useMe } from "../../session";

/** Помощник включён и доступен (иначе его кнопок и раздела нет). */
export function useAssist(enabled = true): boolean {
  const me = useMe().data;
  const q = useQuery({
    queryKey: ["assist"],
    queryFn: () => ok<AssistStatus>(api.GET("/api/helpdesk/assist")),
    enabled: enabled && can(me, "helpdesk.assist.use"),
    staleTime: 300_000,
    retry: false,
  });
  return q.data?.enabled === true;
}
