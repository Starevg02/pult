// Каталог AD в карточке заявки: запрос доступа, учётка заявителя, операции записи.
// Любая запись в AD — с подтверждением и основанием; выполняет её сервер фоновой задачей.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type AccessRequestView, type AccountView, type OperationKind, type OperationView, type TicketDetail } from "../../api/client";
import { formatDate } from "../../format";
import { can, useMe } from "../../session";
import { Badge, Button, CopyButton, Dialog, ErrorBox, Field, Input } from "../../ui";
import { accessTone, KIND_KEY, KIND_PERM } from "./directoryUtil";
import { dyn, type Tone } from "./helpdeskUtil";

const opTone: Record<string, Tone> = { queued: "muted", running: "accent", done: "ok", failed: "bad" };

const newKey = () => (globalThis.crypto?.randomUUID ? globalThis.crypto.randomUUID() : `${Date.now()}-${Math.random().toString(36).slice(2)}`);

function Section({ title, action, children }: { title: string; action?: React.ReactNode; children: React.ReactNode }) {
  return (
    <div className="mt-3 border-t border-line pt-2 text-sm">
      <div className="mb-1 flex items-center gap-2">
        <span className="font-medium">{title}</span>
        <span className="ml-auto">{action}</span>
      </div>
      {children}
    </div>
  );
}

function useRefreshTicket(n: number) {
  const qc = useQueryClient();
  return () => {
    void qc.invalidateQueries({ queryKey: ["ticket", n] });
    void qc.invalidateQueries({ queryKey: ["ticket-access", n] });
    void qc.invalidateQueries({ queryKey: ["directory-ops", n] });
  };
}

// ------------------------------------------------------------------ Запрос доступа

export function AccessPanel({ d }: { d: TicketDetail }) {
  const { t, i18n } = useTranslation();
  const n = d.ticket.number;
  const refresh = useRefreshTicket(n);
  const q = useQuery({
    queryKey: ["ticket-access", n],
    queryFn: () => ok<AccessRequestView | null>(api.GET("/api/helpdesk/tickets/{number}/access", { params: { path: { number: n } } })),
  });
  const send = useMutation({
    mutationFn: () => ok(api.POST("/api/helpdesk/tickets/{number}/access/send", { params: { path: { number: n } } })),
    onSuccess: refresh,
  });
  const execute = useMutation({
    mutationFn: () => ok(api.POST("/api/helpdesk/tickets/{number}/access/execute", { params: { path: { number: n } } })),
    onSuccess: refresh,
  });
  const [confirm, setConfirm] = useState(false);
  const r = q.data;
  if (!r) return null;
  const open = !["resolved", "closed", "canceled"].includes(d.ticket.status);
  return (
    <Section title={t("access.panel")} action={<Badge tone={accessTone[r.status] ?? "muted"}>{dyn(t, `access.status.${r.status}`)}</Badge>}>
      <div className="font-medium">{r.resource_name}</div>
      <div className="text-xs text-muted">{dyn(t, `access.kind.${r.resource_kind}`)}</div>
      {r.approver_name && (
        <div className="mt-1 text-xs text-muted">
          {t("access.approver")}: {r.approver_name}
        </div>
      )}
      {r.decided_at && (
        <div className="text-xs text-muted">
          {t("access.decided")}: {r.decided_by_name ?? ""} {formatDate(r.decided_at, i18n.language)}
        </div>
      )}
      {r.decision_comment && <p className="mt-1 whitespace-pre-wrap text-xs">{r.decision_comment}</p>}
      {r.granted_at && (
        <div className="text-xs text-ok">
          {t("access.granted")}: {formatDate(r.granted_at, i18n.language)}
        </div>
      )}
      {d.can_work && open && (
        <div className="mt-2 flex flex-wrap gap-2">
          {(r.status === "new" || r.status === "rejected") && (
            <Button onClick={() => send.mutate()} disabled={send.isPending}>
              {t("access.send")}
            </Button>
          )}
          {r.status === "approved" && r.resource_kind === "ad_group" && (
            <Button variant="primary" onClick={() => setConfirm(true)} disabled={execute.isPending}>
              {t("access.execute")}
            </Button>
          )}
        </div>
      )}
      {(send.error || execute.error) && <ErrorBox error={send.error ?? execute.error} />}
      <Dialog open={confirm} onClose={() => setConfirm(false)} title={t("access.execute")}>
        <p className="text-sm">{t("access.executeConfirm", { resource: r.resource_name, person: r.person_name ?? "—" })}</p>
        <div className="mt-4 flex justify-end gap-2">
          <Button onClick={() => setConfirm(false)}>{t("common.cancel")}</Button>
          <Button
            variant="primary"
            disabled={execute.isPending}
            onClick={() => {
              execute.mutate();
              setConfirm(false);
            }}
          >
            {t("directory.confirm")}
          </Button>
        </div>
      </Dialog>
    </Section>
  );
}

// ------------------------------------------------------------------ Учётка заявителя

type Pending = { kind: OperationKind; key: string } | null;

export function AccountPanel({ d }: { d: TicketDetail }) {
  const { t, i18n } = useTranslation();
  const me = useMe().data;
  const person = d.ticket.requester_person_id;
  const [show, setShow] = useState(false);
  const [pending, setPending] = useState<Pending>(null);
  const enabled = !!person && d.can_work && can(me, "directory.account.view");
  const q = useQuery({
    queryKey: ["ad-account", person],
    queryFn: () => ok<AccountView>(api.GET("/api/directory/persons/{id}/account", { params: { path: { id: person ?? "" } } })),
    enabled: enabled && show,
    retry: false,
  });
  if (!enabled || !person) return null;
  const a = q.data;
  const start = (kind: OperationKind) => setPending({ kind, key: newKey() });
  const allowed = (k: OperationKind) => can(me, KIND_PERM[k]);
  return (
    <Section
      title={t("directory.account")}
      action={
        !show ? (
          <Button variant="ghost" onClick={() => setShow(true)}>
            {t("directory.readAccount")}
          </Button>
        ) : (
          <Button variant="ghost" onClick={() => void q.refetch()} disabled={q.isFetching}>
            {t("directory.refresh")}
          </Button>
        )
      }
    >
      {show && q.isPending && <div className="text-xs text-muted">{t("common.loading")}</div>}
      {q.error && <ErrorBox error={q.error} />}
      {a && (
        <>
          <div className="font-mono text-xs">{a.login}</div>
          <div className="mt-1 flex flex-wrap gap-1">
            <Badge tone={a.enabled ? "ok" : "bad"}>{a.enabled ? t("directory.enabled") : t("directory.disabled")}</Badge>
            {a.locked && <Badge tone="bad">{t("directory.locked")}</Badge>}
            {a.must_change_password && <Badge tone="accent">{t("directory.mustChange")}</Badge>}
          </div>
          <dl className="mt-1 space-y-0.5 text-xs text-muted">
            {a.password_last_set_at && (
              <div>
                {t("directory.passwordSet")}: {formatDate(a.password_last_set_at, i18n.language)}
              </div>
            )}
            {a.password_expires_at && (
              <div>
                {t("directory.passwordExpires")}: {formatDate(a.password_expires_at, i18n.language)}
              </div>
            )}
            {a.last_logon_at && (
              <div>
                {t("directory.lastLogon")}: {formatDate(a.last_logon_at, i18n.language)}
              </div>
            )}
            <div>{t("directory.groupsCount", { n: a.groups.length })}</div>
          </dl>
          <div className="mt-2 flex flex-wrap gap-1">
            {a.locked && allowed("user.unlock") && <Button onClick={() => start("user.unlock")}>{t("directory.kind.unlock")}</Button>}
            {!a.enabled && allowed("user.enable") && <Button onClick={() => start("user.enable")}>{t("directory.kind.enable")}</Button>}
            {allowed("user.reset_password") && <Button onClick={() => start("user.reset_password")}>{t("directory.kind.resetPassword")}</Button>}
            {allowed("group.add_member") && <Button onClick={() => start("group.add_member")}>{t("directory.kind.addMember")}</Button>}
          </div>
        </>
      )}
      {pending && <OperationDialog d={d} person={person} login={a?.login ?? ""} pending={pending} onClose={() => setPending(null)} />}
    </Section>
  );
}

function OperationDialog({ d, person, login, pending, onClose }: { d: TicketDetail; person: string; login: string; pending: NonNullable<Pending>; onClose: () => void }) {
  const { t } = useTranslation();
  const n = d.ticket.number;
  const refresh = useRefreshTicket(n);
  const [basis, setBasis] = useState(t("directory.basisDefault", { n }));
  const [group, setGroup] = useState("");
  const [sure, setSure] = useState(false);
  const isGroup = pending.kind === "group.add_member";
  const create = useMutation({
    mutationFn: () =>
      ok<OperationView>(
        api.POST("/api/directory/operations", {
          body: {
            kind: pending.kind,
            person_id: person,
            group_dn: isGroup ? group.trim() : null,
            basis: basis.trim(),
            ticket_number: n,
            idempotency_key: pending.key,
            confirm: true,
          },
        }),
      ),
    onSuccess: () => {
      refresh();
      onClose();
    },
  });
  const label = dyn(t, `directory.kind.${KIND_KEY[pending.kind]}`);
  return (
    <Dialog open onClose={onClose} title={label}>
      <div className="space-y-3 text-sm">
        <p>{t("directory.confirmText", { action: label, login })}</p>
        {pending.kind === "user.reset_password" && <p className="text-xs text-muted">{t("directory.resetHint")}</p>}
        {isGroup && (
          <Field label={t("directory.groupDn")} hint={t("directory.groupDnHint")}>
            <Input value={group} onChange={(e) => setGroup(e.target.value)} placeholder="CN=…,OU=…,DC=…" />
          </Field>
        )}
        <Field label={t("directory.basis")}>
          <Input value={basis} maxLength={2000} onChange={(e) => setBasis(e.target.value)} />
        </Field>
        <label className="flex items-center gap-2">
          <input type="checkbox" checked={sure} onChange={(e) => setSure(e.target.checked)} />
          {t("directory.sure")}
        </label>
        {create.error && <ErrorBox error={create.error} />}
        <div className="flex justify-end gap-2">
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button variant="primary" disabled={!sure || !basis.trim() || (isGroup && !group.trim()) || create.isPending} onClick={() => create.mutate()}>
            {t("directory.confirm")}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}

// ------------------------------------------------------------------ Пункт чек-листа с действием каталога

export function ChecklistRunDialog({ number, item, action, title, onClose }: { number: number; item: string; action: string; title: string; onClose: () => void }) {
  const { t } = useTranslation();
  const refresh = useRefreshTicket(number);
  const [login, setLogin] = useState("");
  const [group, setGroup] = useState("");
  const [sure, setSure] = useState(false);
  const isGroup = action === "directory.add_member";
  const run = useMutation({
    mutationFn: () =>
      ok(
        api.POST("/api/helpdesk/tickets/{number}/checklist/{item}/run", {
          params: { path: { number, item } },
          body: { login: login.trim(), group_dn: isGroup ? group.trim() : null },
        }),
      ),
    onSuccess: () => {
      refresh();
      onClose();
    },
  });
  return (
    <Dialog open onClose={onClose} title={title}>
      <div className="space-y-3 text-sm">
        <Field label={t("directory.login")} hint={t("directory.loginHint")}>
          <Input autoFocus value={login} onChange={(e) => setLogin(e.target.value)} placeholder="ivanov" />
        </Field>
        {isGroup && (
          <Field label={t("directory.groupDn")} hint={t("directory.groupDnHint")}>
            <Input value={group} onChange={(e) => setGroup(e.target.value)} placeholder="CN=…,OU=…,DC=…" />
          </Field>
        )}
        <label className="flex items-center gap-2">
          <input type="checkbox" checked={sure} onChange={(e) => setSure(e.target.checked)} />
          {t("directory.sure")}
        </label>
        {run.error && <ErrorBox error={run.error} />}
        <div className="flex justify-end gap-2">
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button variant="primary" disabled={!sure || !login.trim() || (isGroup && !group.trim()) || run.isPending} onClick={() => run.mutate()}>
            {t("directory.confirm")}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}

// ------------------------------------------------------------------ Операции заявки

export function Operations({ d }: { d: TicketDetail }) {
  const { t, i18n } = useTranslation();
  const me = useMe().data;
  const n = d.ticket.number;
  const q = useQuery({
    queryKey: ["directory-ops", n],
    queryFn: () => ok<OperationView[]>(api.GET("/api/directory/operations", { params: { query: { ticket: n } } })),
    enabled: d.can_work && can(me, "directory.account.view"),
    refetchInterval: (s) => (s.state.data?.some((o) => o.status === "queued" || o.status === "running") ? 2000 : false),
  });
  const [shown, setShown] = useState<string | null>(null);
  const ops = q.data ?? [];
  // Операция завершилась — состояние учётки в карточке устарело: перечитать его из AD,
  // иначе после разблокировки остаются «Заблокирована» и кнопка «Разблокировать».
  const qc = useQueryClient();
  const finished = ops
    .filter((o) => o.status === "done" || o.status === "failed")
    .map((o) => o.id)
    .join(",");
  const seen = useRef<string | null>(null);
  useEffect(() => {
    if (!q.data) return;
    if (seen.current !== null && seen.current !== finished) void qc.invalidateQueries({ queryKey: ["ad-account"] });
    seen.current = finished;
  }, [finished, q.data, qc]);
  if (ops.length === 0) return null;
  return (
    <Section title={t("directory.operations")}>
      <ul className="space-y-2">
        {ops.map((o) => (
          <li key={o.id}>
            <div className="flex items-center gap-2">
              <span className="min-w-0 flex-1 truncate">
                {dyn(t, `directory.kind.${KIND_KEY[o.kind] ?? "unknown"}`)} · <span className="font-mono text-xs">{o.login}</span>
              </span>
              <Badge tone={opTone[o.status] ?? "muted"}>{dyn(t, `directory.status.${o.status}`)}</Badge>
            </div>
            {o.group_dn && <div className="truncate font-mono text-xs text-muted">{o.group_dn}</div>}
            <div className="text-xs text-muted">
              {o.requested_by_name ?? ""} {formatDate(o.created_at, i18n.language)}
              {o.changed === false && ` · ${t("directory.unchanged")}`}
            </div>
            {o.error && <div className="text-xs text-bad">{o.error}</div>}
            {o.has_password && o.requested_by === me?.user_id && (
              <Button className="mt-1" onClick={() => setShown(o.id)}>
                {t("directory.showPassword")}
              </Button>
            )}
          </li>
        ))}
      </ul>
      {shown && <PasswordDialog op={shown} number={n} onClose={() => setShown(null)} />}
    </Section>
  );
}

/** Временный пароль показывается один раз: после закрытия окна его больше не получить. */
function PasswordDialog({ op, number, onClose }: { op: string; number: number; onClose: () => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const reveal = useMutation({
    mutationFn: () => ok<{ password: string }>(api.POST("/api/directory/operations/{id}/password", { params: { path: { id: op } } })),
    onSettled: () => void qc.invalidateQueries({ queryKey: ["directory-ops", number] }),
  });
  const close = () => {
    reveal.reset();
    onClose();
  };
  return (
    <Dialog open onClose={close} title={t("directory.tempPassword")}>
      <div className="space-y-3 text-sm">
        <p>{t("directory.onceHint")}</p>
        {reveal.data ? (
          <div className="flex items-center gap-2 rounded-lg border border-line bg-sunken px-3 py-2">
            <span className="font-mono text-base select-all">{reveal.data.password}</span>
            <CopyButton value={reveal.data.password} label={t("common.copy")} />
          </div>
        ) : (
          <Button variant="primary" onClick={() => reveal.mutate()} disabled={reveal.isPending}>
            {t("directory.showPassword")}
          </Button>
        )}
        {reveal.error && <ErrorBox error={reveal.error} />}
        <div className="flex justify-end">
          <Button onClick={close}>{t("common.close")}</Button>
        </div>
      </div>
    </Dialog>
  );
}
