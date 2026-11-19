// Общее для блоков карточки заявки: справочники, запросы, форматирование сроков.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, ok, type CloseCode, type FoundObject, type Group } from "../../api/client";

export const IMPACTS = ["person", "department", "organization"] as const;

export const URGENCIES = ["normal", "hinders", "blocked"] as const;

export const OBJECT_KINDS = ["computer", "person", "ad_account", "asset"] as const;

/** Обновить карточку и списки после действия. */
export function useTicketMutation<T>(number: number, fn: (v: T) => Promise<unknown>, after?: () => void) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: fn,
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["ticket", number] });
      void qc.invalidateQueries({ queryKey: ["tickets"] });
      after?.();
    },
  });
}

/** Сколько осталось до срока: «2 ч 15 мин» или «просрочено на 20 мин». */
export function dueText(t: (k: never, o?: object) => string, iso: string | null | undefined, now = Date.now()): string {
  if (!iso) return "";
  const diff = Math.round((new Date(iso).getTime() - now) / 60_000);
  const abs = Math.abs(diff);
  const h = Math.floor(abs / 60);
  const m = abs % 60;
  const span = h > 0 ? t("card.hm" as never, { h, m }) : t("card.m" as never, { m });
  return diff >= 0 ? t("card.left" as never, { span }) : t("card.overdue" as never, { span });
}

/** Оборудование сотрудника: подсказка инженеру (заявка за сотрудника, карточка заявки). */
export function usePersonAssets(person: string | null | undefined) {
  return useQuery({
    queryKey: ["person-assets", person],
    queryFn: () => ok<FoundObject[]>(api.GET("/api/helpdesk/persons/{id}/assets", { params: { path: { id: person ?? "" } } })),
    enabled: !!person,
  });
}

export function useCloseCodes() {
  return useQuery({ queryKey: ["close-codes"], queryFn: () => ok<CloseCode[]>(api.GET("/api/helpdesk/close-codes")), staleTime: 300_000 });
}

export function useGroups(enabled = true) {
  return useQuery({ queryKey: ["groups"], queryFn: () => ok<Group[]>(api.GET("/api/helpdesk/groups")), enabled, staleTime: 60_000 });
}

export const minutesOf = (v: string) => (v ? Number(v) : null);

export function closeCodeLabel(codes: CloseCode[] | undefined, code: string | null | undefined) {
  return codes?.find((c) => c.code === code)?.label ?? code ?? "";
}
