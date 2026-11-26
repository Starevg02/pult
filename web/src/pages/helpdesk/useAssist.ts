// Включён ли помощник: от этого зависят его раздел и кнопки.
import { useQuery } from "@tanstack/react-query";
import { api, ok, type AssistStatus } from "../../api/client";
import { can, useMe } from "../../session";

/** Состояние помощника: включён ли он и идёт ли ещё запрос (тогда «выключен» показывать рано). */
export function useAssistState(enabled = true): { enabled: boolean; pending: boolean } {
  const me = useMe().data;
  const allowed = enabled && can(me, "helpdesk.assist.use");
  const q = useQuery({
    queryKey: ["assist"],
    queryFn: () => ok<AssistStatus>(api.GET("/api/helpdesk/assist")),
    enabled: allowed,
    staleTime: 300_000,
    retry: false,
  });
  return { enabled: q.data?.enabled === true, pending: allowed && q.isPending };
}

/** Помощник включён и доступен (иначе его кнопок и раздела нет). */
export function useAssist(enabled = true): boolean {
  return useAssistState(enabled).enabled;
}
