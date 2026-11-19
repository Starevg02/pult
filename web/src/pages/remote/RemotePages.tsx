// Компьютеры с агентом: список, «где залогинен», карточка ПК.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type DeviceCard, type DeviceRow, type SessionRow } from "../../api/client";
import { formatDate } from "../../format";
import { can, useMe } from "../../session";
import { Badge, Button, Card, Empty, ErrorBox, Input, PageHeader, Spinner } from "../../ui";
import { dyn } from "../helpdesk/helpdeskUtil";
import { StatusBadge, TicketLink } from "../helpdesk/parts";
import { ConnectButtons } from "./connect";

function useDebounced(value: string, ms = 250) {
  const [v, setV] = useState(value);
  useEffect(() => {
    const h = setTimeout(() => setV(value.trim()), ms);
    return () => clearTimeout(h);
  }, [value, ms]);
  return v;
}

export function Online({ online }: { online: boolean }) {
  const { t } = useTranslation();
  return <Badge tone={online ? "ok" : "muted"}>{online ? t("remote.online") : t("remote.offline")}</Badge>;
}

function DeviceLink({ id, children }: { id: string; children: React.ReactNode }) {
  return (
    <Link to="/computers/$id" params={{ id }} className="font-mono font-medium text-accent hover:underline">
      {children}
    </Link>
  );
}

/** Сессии пользователя (или поиска) с кнопкой подключения к каждой. */
export function SessionList({ sessions, showPc = true }: { sessions: SessionRow[]; showPc?: boolean }) {
  const { t, i18n } = useTranslation();
  return (
    <ul className="divide-y divide-line">
      {sessions.map((s) => (
        <li key={`${s.device_id}-${s.session_id}`} className="space-y-2 py-2 text-sm">
          <div className="flex flex-wrap items-center gap-2">
            {showPc && <DeviceLink id={s.device_id}>{s.hostname}</DeviceLink>}
            <span className="font-medium">{s.display_name ?? s.username}</span>
            <span className="font-mono text-xs text-muted">{s.user_domain ? `${s.user_domain}\\${s.username}` : s.username}</span>
            <Badge tone={s.state === "active" ? "ok" : "muted"}>{dyn(t, `remote.sessionState.${s.state}`)}</Badge>
            {!s.online && <Online online={false} />}
            <span className="ml-auto text-xs text-muted">
              {t("remote.session", { id: s.session_id })}
              {s.logon_at && ` · ${t("remote.logon")} ${formatDate(s.logon_at, i18n.language)}`}
            </span>
          </div>
          {s.online && <ConnectButtons compact target={{ device_id: s.device_id, session_id: s.session_id }} />}
        </li>
      ))}
    </ul>
  );
}

export function ComputersPage() {
  const { t, i18n } = useTranslation();
  const [q, setQ] = useState("");
  const [onlineOnly, setOnlineOnly] = useState(false);
  const [who, setWho] = useState("");
  const needle = useDebounced(q);
  const person = useDebounced(who);
  const devices = useQuery({
    queryKey: ["remote-devices", needle, onlineOnly],
    queryFn: () => ok<DeviceRow[]>(api.GET("/api/remote/devices", { params: { query: { q: needle || undefined, online: onlineOnly || undefined, limit: 200 } } })),
  });
  const where = useQuery({
    queryKey: ["remote-where", person],
    queryFn: () => ok<SessionRow[]>(api.GET("/api/remote/sessions", { params: { query: { q: person } } })),
    enabled: person.length > 1,
  });
  return (
    <div className="mx-auto max-w-5xl space-y-4">
      <PageHeader title={t("remote.title")} />
      <Card className="p-4">
        <h2 className="mb-2 text-sm font-semibold">{t("remote.whereTitle")}</h2>
        <Input type="search" value={who} onChange={(e) => setWho(e.target.value)} placeholder={t("remote.wherePlaceholder")} />
        {where.error && <ErrorBox error={where.error} />}
        {where.data && (where.data.length === 0 ? <Empty>{t("remote.whereEmpty")}</Empty> : <SessionList sessions={where.data} />)}
      </Card>
      <div className="flex flex-wrap items-center gap-3">
        <Input className="max-w-sm" type="search" value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("remote.search")} />
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={onlineOnly} onChange={(e) => setOnlineOnly(e.target.checked)} />
          {t("remote.onlineOnly")}
        </label>
        {devices.data && <span className="ml-auto text-xs text-muted">{t("remote.count", { n: devices.data.length })}</span>}
      </div>
      {devices.error && <ErrorBox error={devices.error} />}
      {devices.isPending ? (
        <Spinner />
      ) : devices.data?.length === 0 ? (
        <Empty>{t("remote.empty")}</Empty>
      ) : (
        <Card className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead className="text-left text-xs text-muted">
              <tr className="border-b border-line">
                <th className="px-3 py-2">{t("remote.col.pc")}</th>
                <th className="px-3 py-2">{t("remote.col.users")}</th>
                <th className="px-3 py-2">{t("remote.col.os")}</th>
                <th className="px-3 py-2">{t("remote.col.seen")}</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-line">
              {devices.data?.map((d) => (
                <tr key={d.id} className={d.is_active ? "" : "opacity-50"}>
                  <td className="px-3 py-2">
                    <div className="flex items-center gap-2">
                      {d.alias && <span className="text-accent">★</span>}
                      <DeviceLink id={d.id}>{d.hostname}</DeviceLink>
                      <Online online={d.online} />
                    </div>
                    {d.alias && <div className="text-xs text-muted">{d.alias}</div>}
                  </td>
                  <td className="px-3 py-2">{d.users.join(", ") || "—"}</td>
                  <td className="px-3 py-2 text-xs text-muted">{d.os_version ?? "—"}</td>
                  <td className="px-3 py-2 text-xs text-muted">{formatDate(d.last_seen_at, i18n.language)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </Card>
      )}
    </div>
  );
}

function Favorite({ d }: { d: DeviceRow }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const m = useMutation({
    mutationFn: () =>
      d.alias != null
        ? ok(api.DELETE("/api/remote/devices/{id}/favorite", { params: { path: { id: d.id } } }))
        : ok(api.PUT("/api/remote/devices/{id}/favorite", { params: { path: { id: d.id } }, body: {} })),
    onSettled: () => void qc.invalidateQueries({ queryKey: ["remote-device", d.id] }),
  });
  const fav = d.alias != null;
  return (
    <button type="button" className="text-lg text-accent" disabled={m.isPending} onClick={() => m.mutate()} aria-label={fav ? t("remote.unfavorite") : t("remote.favorite")} title={fav ? t("remote.unfavorite") : t("remote.favorite")}>
      {fav ? "★" : "☆"}
    </button>
  );
}

export function DevicePage() {
  const { t, i18n } = useTranslation();
  const me = useMe().data;
  const { id } = useParams({ from: "/computers/$id" });
  const q = useQuery({ queryKey: ["remote-device", id], queryFn: () => ok<DeviceCard>(api.GET("/api/remote/devices/{id}", { params: { path: { id } } })) });
  if (q.isPending) return <Spinner />;
  if (q.error) return <ErrorBox error={q.error} />;
  const c = q.data;
  const d = c.device;
  const denied = c.rules.includes("deny");
  const prop = (label: string, value: React.ReactNode) =>
    value ? (
      <div className="flex gap-3 py-1 text-sm">
        <dt className="w-36 shrink-0 text-muted">{label}</dt>
        <dd className="min-w-0 break-words">{value}</dd>
      </div>
    ) : null;
  return (
    <div className="mx-auto max-w-5xl space-y-4">
      <PageHeader title={d.hostname}>
        <Favorite d={d} />
        <Online online={d.online} />
        {!d.is_active && <Badge tone="bad">{t("remote.inactive")}</Badge>}
        {c.rules.map((r) => (
          <Badge key={r} tone="bad">
            {dyn(t, `remote.rule.${r}`)}
          </Badge>
        ))}
      </PageHeader>
      <div className="grid gap-4 md:grid-cols-2">
        <Card className="p-4">
          <dl>
            {prop(t("remote.prop.fqdn"), d.fqdn && <span className="font-mono">{d.fqdn}</span>)}
            {prop(t("remote.prop.description"), c.description)}
            {prop(t("remote.prop.os"), d.os_version ?? c.os)}
            {prop(t("remote.prop.ip"), d.ip_addrs.length > 0 && <span className="font-mono">{d.ip_addrs.join(", ")}</span>)}
            {prop(t("remote.prop.agent"), d.agent_version)}
            {prop(t("remote.prop.seen"), formatDate(d.last_seen_at, i18n.language))}
            {prop(t("remote.prop.enrolled"), formatDate(c.enrolled_at, i18n.language))}
          </dl>
        </Card>
        <Card className="p-4">
          <h2 className="mb-2 text-sm font-semibold">{t("remote.sessions")}</h2>
          {denied ? (
            <p className="text-sm text-bad">{t("remote.deniedHint")}</p>
          ) : c.sessions.length === 0 ? (
            <Empty>{t("remote.noSessions")}</Empty>
          ) : (
            <SessionList sessions={c.sessions} showPc={false} />
          )}
          {!denied && d.online && can(me, "remote.connect.control") && (
            <div className="mt-3 border-t border-line pt-3">
              <div className="mb-2 text-xs text-muted">{t("remote.quickTitle")}</div>
              <QuickOnly deviceId={d.id} />
            </div>
          )}
        </Card>
      </div>
      {c.tickets.length > 0 && (
        <Card className="p-4">
          <h2 className="mb-2 text-sm font-semibold">{t("remote.tickets")}</h2>
          <ul className="space-y-1 text-sm">
            {c.tickets.map((x) => (
              <li key={x.number} className="flex items-center gap-2">
                <TicketLink number={x.number} className="font-mono text-xs text-accent">
                  #{x.number}
                </TicketLink>
                <span className="min-w-0 flex-1 truncate">{x.title}</span>
                <StatusBadge status={x.status} />
                <span className="text-xs text-muted">{formatDate(x.created_at, i18n.language)}</span>
              </li>
            ))}
          </ul>
        </Card>
      )}
      <Card className="p-4">
        <h2 className="mb-2 text-sm font-semibold">{t("remote.history")}</h2>
        {c.connections.length === 0 ? (
          <Empty>{t("remote.noHistory")}</Empty>
        ) : (
          <ul className="divide-y divide-line text-sm">
            {c.connections.map((x) => (
              <li key={x.id} className="flex flex-wrap items-center gap-2 py-1.5">
                <span className="text-xs text-muted">{formatDate(x.created_at, i18n.language)}</span>
                <span>{x.operator}</span>
                <span className="font-medium">{dyn(t, `remote.action.${x.action}`)}</span>
                {x.mode && <Badge>{dyn(t, `remote.mode.${x.mode}`)}</Badge>}
                {x.target_user && <span className="text-muted">{x.target_user}</span>}
                {x.ticket_number && (
                  <TicketLink number={x.ticket_number} className="font-mono text-xs text-accent">
                    #{x.ticket_number}
                  </TicketLink>
                )}
                <Badge tone={x.status === "started" ? "ok" : x.status === "failed" ? "bad" : "muted"}>{dyn(t, `remote.status.${x.status}`)}</Badge>
                {x.error && <span className="text-xs text-bad">{x.error}</span>}
              </li>
            ))}
          </ul>
        )}
      </Card>
    </div>
  );
}

/** Быстрые действия без выбора сессии (диск, управление, PowerShell, проверка). */
function QuickOnly({ deviceId }: { deviceId: string }) {
  const [open, setOpen] = useState(false);
  const { t } = useTranslation();
  if (!open) return <Button variant="ghost" onClick={() => setOpen(true)}>{t("remote.connect.more")} ▾</Button>;
  return <ConnectButtons target={{ device_id: deviceId }} />;
}

/** Где сейчас залогинен сотрудник (карточка в справочнике). */
export function PersonSessions({ personId }: { personId: string }) {
  const { t } = useTranslation();
  const me = useMe().data;
  const q = useQuery({
    queryKey: ["remote-person", personId],
    queryFn: () => ok<SessionRow[]>(api.GET("/api/remote/persons/{id}/sessions", { params: { path: { id: personId } } })),
    enabled: can(me, "remote.device.view"),
  });
  if (!can(me, "remote.device.view") || !q.data) return null;
  return (
    <div className="mt-4 border-t border-line pt-3">
      <div className="mb-1 text-xs text-muted">{t("remote.loggedOn")}</div>
      {q.data.length === 0 ? <div className="text-sm text-muted">{t("remote.notLoggedOn")}</div> : <SessionList sessions={q.data} />}
    </div>
  );
}

/** Карточка заявки: ПК заявителя и подключение одним нажатием. */
export function TicketRemote({ ticket, personId, computer }: { ticket: number; personId?: string | null; computer?: string | null }) {
  const { t } = useTranslation();
  const me = useMe().data;
  const show = can(me, "remote.connect.view");
  const q = useQuery({
    queryKey: ["remote-person", personId],
    queryFn: () => ok<SessionRow[]>(api.GET("/api/remote/persons/{id}/sessions", { params: { path: { id: personId ?? "" } } })),
    enabled: show && !!personId,
  });
  if (!show || (!computer && !q.data?.length)) return null;
  const online = q.data?.filter((s) => s.online) ?? [];
  return (
    <div className="mt-3 border-t border-line pt-2 text-sm">
      <div className="mb-1 font-medium">{t("remote.ticketTitle")}</div>
      {computer && <ConnectButtons target={{ ticket_number: ticket }} />}
      {online.filter((s) => s.hostname.toLowerCase() !== computer?.toLowerCase()).length > 0 && (
        <div className="mt-2">
          <div className="text-xs text-muted">{t("remote.otherSessions")}</div>
          <SessionList sessions={online.filter((s) => s.hostname.toLowerCase() !== computer?.toLowerCase())} />
        </div>
      )}
    </div>
  );
}
