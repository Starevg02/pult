// Окно звонка: звонок идёт с сервера портала, состояние — опросом раз в секунду.
import { useMutation, useQuery } from "@tanstack/react-query";
import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { api, ok, type Person } from "../api/client";
import { Button, Dialog, ErrorBox } from "../ui";
import { fullName } from "./phonebookUtil";
import { dyn } from "./helpdesk/helpdeskUtil";

/** Подпись состояния; незнакомое значение шлюза — как есть. */
const stateLabel = (t: TFunction, s: string) => {
  const k = `call.states.${s}`;
  const r = dyn(t, k);
  return r === k ? s : r;
};

const FINAL = ["finished", "failed", "ended", "completed"];

export function CallDialog({ person, onClose }: { person: Person; onClose: () => void }) {
  const { t } = useTranslation();
  const start = useMutation({ mutationFn: () => ok(api.POST("/api/phonebook/calls", { body: { person_id: person.id } })) });
  const id = start.data?.id;
  const state = useQuery({
    queryKey: ["call", id],
    queryFn: () => ok(api.GET("/api/phonebook/calls/{id}", { params: { path: { id: id ?? "" } } })),
    enabled: !!id,
    refetchInterval: (q) => (q.state.data && (q.state.data.done || FINAL.includes(q.state.data.state)) ? false : 1000),
  });
  const { mutate } = start;
  useEffect(() => mutate(), [mutate]);
  const s = state.data?.state ?? (start.isPending ? "calling" : undefined);
  return (
    <Dialog open onClose={onClose} title={t("call.title", { name: fullName(person) })}>
      <p className="mb-1 font-mono text-lg">{person.internal_ext}</p>
      {s && <p className="mb-4 text-sm text-muted">{t("call.state", { state: stateLabel(t, s) })}</p>}
      {state.data?.error && <p className="mb-4 text-sm text-bad">{state.data.error}</p>}
      {start.error && <ErrorBox error={start.error} />}
      {state.error && <ErrorBox error={state.error} />}
      <div className="mt-4 text-right">
        <Button onClick={onClose}>{t("common.close")}</Button>
      </div>
    </Dialog>
  );
}
