// Подключение к ПК: портал выдаёт одноразовую ссылку pult://, браузер открывает
// helper на ПК инженера, helper сообщает итог — карточка показывает его, опрашивая подключение.
import { useMutation, useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type ConnectionView, type IssuedConnection, type NewConnection, type RemoteAction } from "../../api/client";
import { can, useMe } from "../../session";
import { Badge, Button, Dialog, ErrorBox } from "../../ui";
import { dyn } from "../helpdesk/helpdeskUtil";

const DONE = ["started", "failed", "expired"];

/** Где взять helper — его выдаёт портал (загружает администратор). */
const HELPER_URL = "/api/remote/packages/helper/download";

function Progress({ issued, onClose }: { issued: IssuedConnection; onClose: () => void }) {
  const { t } = useTranslation();
  const st = useQuery({
    queryKey: ["remote-connection", issued.id],
    queryFn: () => ok<ConnectionView>(api.GET("/api/remote/connections/{id}", { params: { path: { id: issued.id } } })),
    refetchInterval: (q) => (q.state.data && DONE.includes(q.state.data.status) ? false : 2000),
  });
  const status = st.data?.status ?? "issued";
  const tone = status === "started" ? "ok" : status === "failed" || status === "expired" ? "bad" : "muted";
  const probe = st.data?.result && issued.action === "probe" ? Object.entries(st.data.result as Record<string, unknown>) : [];
  return (
    <Dialog open onClose={onClose} title={t("remote.connect.title", { pc: issued.hostname })}>
      <div className="space-y-3 text-sm">
        <div className="flex flex-wrap items-center gap-2">
          <span className="font-medium">{dyn(t, `remote.action.${issued.action}`)}</span>
          {issued.mode && <Badge>{dyn(t, `remote.mode.${issued.mode}`)}</Badge>}
          {issued.consent && <Badge tone={issued.consent === "prompted" ? "accent" : "muted"}>{dyn(t, `remote.consent.${issued.consent}`)}</Badge>}
          {issued.target_user && <span className="text-muted">{issued.target_user}</span>}
        </div>
        <div className="flex items-center gap-2">
          <Badge tone={tone}>{dyn(t, `remote.status.${status}`)}</Badge>
          <span className="text-muted">{dyn(t, `remote.statusHint.${status}`)}</span>
        </div>
        {st.data?.error && <div className="rounded-lg bg-bad/5 px-3 py-2 text-bad">{st.data.error}</div>}
        {probe.length > 0 && (
          <ul className="grid grid-cols-2 gap-1 font-mono text-xs">
            {probe.map(([k, v]) => (
              <li key={k}>
                {k}: <span className={v ? "text-ok" : "text-bad"}>{v ? "✓" : "✗"}</span>
              </li>
            ))}
          </ul>
        )}
        {(status === "issued" || status === "expired") && (
          <p className="text-xs text-muted">
            {t("remote.connect.noHelper")}{" "}
            <a className="text-accent underline" href={HELPER_URL}>
              {t("remote.connect.getHelper")}
            </a>
          </p>
        )}
      </div>
      <div className="mt-4 flex justify-end gap-2">
        {status === "issued" && (
          <Button onClick={() => openLink(issued.link)}>{t("remote.connect.again")}</Button>
        )}
        <Button variant="primary" onClick={onClose}>
          {t("common.close")}
        </Button>
      </div>
    </Dialog>
  );
}

/** Ссылка от сервера открывается, только если это схема helper: иначе ответ сервера мог бы увести страницу. */
function openLink(link: string) {
  if (link.startsWith("pult://")) window.location.assign(link);
}

/** Запрос подключения → ссылка pult:// → окно с ходом подключения. */
function useConnect() {
  const [issued, setIssued] = useState<IssuedConnection | null>(null);
  const m = useMutation({
    mutationFn: (body: NewConnection) => ok<IssuedConnection>(api.POST("/api/remote/connections", { body })),
    onSuccess: (r) => {
      setIssued(r);
      openLink(r.link);
    },
  });
  const dialog = issued && <Progress issued={issued} onClose={() => setIssued(null)} />;
  return { connect: m.mutate, pending: m.isPending, error: m.error, reset: m.reset, dialog };
}

const QUICK: RemoteAction[] = ["probe", "files", "manage", "powershell"];

/**
 * Кнопки подключения: просмотр (всегда с согласием), управление и быстрые действия — по правам.
 * `target` — ПК и сессия, либо заявка (ПК и сессия заявителя определяются на сервере).
 */
export function ConnectButtons({ target, compact }: { target: Pick<NewConnection, "device_id" | "ticket_number" | "session_id">; compact?: boolean }) {
  const { t } = useTranslation();
  const me = useMe().data;
  const c = useConnect();
  const [more, setMore] = useState(false);
  const control = can(me, "remote.connect.control");
  if (!can(me, "remote.connect.view")) return null;
  const go = (b: Omit<NewConnection, "device_id" | "ticket_number" | "session_id">) => {
    setMore(false);
    // Сессия нужна только теневому подключению.
    c.connect({ ...target, ...b, session_id: b.action === "shadow" ? target.session_id : null });
  };
  return (
    <div className="space-y-2">
      <div className="flex flex-wrap gap-2">
        <Button variant="primary" disabled={c.pending} onClick={() => go({ action: "shadow", mode: "view" })}>
          {compact ? t("remote.connect.view") : t("remote.connect.viewLong")}
        </Button>
        {control && (
          <Button disabled={c.pending} onClick={() => go({ action: "shadow", mode: "control" })}>
            {t("remote.connect.control")}
          </Button>
        )}
        {control && (
          <Button variant="ghost" disabled={c.pending} title={t("remote.connect.silentHint")} onClick={() => window.confirm(t("remote.connect.silentConfirm")) && go({ action: "shadow", mode: "control", skip_consent: true })}>
            {t("remote.connect.silent")}
          </Button>
        )}
        {(target.device_id || target.ticket_number) && (
          <Button variant="ghost" aria-expanded={more} onClick={() => setMore(!more)}>
            {t("remote.connect.more")} ▾
          </Button>
        )}
      </div>
      {more && (
        <div className="flex flex-wrap gap-2">
          {QUICK.filter((a) => a === "probe" || control).map((a) => (
            <Button key={a} variant="secondary" disabled={c.pending} onClick={() => go({ action: a })}>
              {dyn(t, `remote.action.${a}`)}
            </Button>
          ))}
        </div>
      )}
      {c.error && <ErrorBox error={c.error} />}
      {c.dialog}
    </div>
  );
}
