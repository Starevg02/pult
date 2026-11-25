// «Состояние сервисов» на главной инженера и открытые оповещения в карточке объекта.
import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useTranslation } from "react-i18next";
import { api, ok, type AssetAlert, type ServiceState } from "../../api/client";
import { formatDate } from "../../format";
import { Badge, Card, ErrorBox, Spinner } from "../../ui";
import { dyn } from "../helpdesk/helpdeskUtil";
import { TicketLink } from "../helpdesk/parts";

const DOT: Record<string, string> = { ok: "bg-ok", info: "bg-muted", warning: "bg-accent", critical: "bg-bad" };

function Cause({ a, own }: { a: AssetAlert; own: boolean }) {
  const { t, i18n } = useTranslation();
  return (
    <li className="text-xs text-muted">
      <Badge tone={a.severity === "critical" ? "bad" : a.severity === "warning" ? "accent" : "muted"}>{dyn(t, `svc.monitoring.severity.${a.severity}`)}</Badge> {a.title}
      {!own && (
        <>
          {" · "}
          <Link to="/objects/$kind/$id" params={{ kind: "asset", id: a.asset_id }} className="text-accent hover:underline">
            {a.asset_name}
          </Link>
        </>
      )}
      {" · "}
      {formatDate(a.started_at, i18n.language)}
      {a.ticket_number != null && (
        <>
          {" · "}
          <TicketLink number={a.ticket_number} className="text-accent hover:underline">
            #{a.ticket_number}
          </TicketLink>
        </>
      )}
    </li>
  );
}

/** Виджет главной: сервисы и их состояние по оповещениям мониторинга. */
export function ServiceStatusCard() {
  const { t } = useTranslation();
  const q = useQuery({
    queryKey: ["service-status"],
    queryFn: () => ok<ServiceState[]>(api.GET("/api/helpdesk/monitoring/services")),
    refetchInterval: 60_000,
  });
  if (q.data?.length === 0) return null;
  const bad = q.data?.filter((s) => s.state !== "ok").length ?? 0;
  return (
    <Card>
      <div className="flex items-center gap-2 border-b border-line px-4 py-3">
        <h2 className="text-sm font-semibold">{t("cmdb.services")}</h2>
        {q.data && (
          <span className={`ml-auto text-xs ${bad ? "text-bad" : "text-ok"}`} role="status">
            {bad ? t("cmdb.servicesBad", { n: bad }) : t("cmdb.servicesOk")}
          </span>
        )}
      </div>
      {q.error ? (
        <ErrorBox error={q.error} />
      ) : !q.data ? (
        <div className="p-4">
          <Spinner />
        </div>
      ) : (
        <ul className="max-h-96 divide-y divide-line overflow-y-auto">
          {q.data.map((s) => (
            <li key={s.id} className="px-4 py-2">
              <div className="flex items-center gap-2 text-sm">
                <span aria-hidden="true" className={`h-2.5 w-2.5 shrink-0 rounded-full ${DOT[s.state] ?? "bg-muted"}`} />
                <Link to="/objects/$kind/$id" params={{ kind: "asset", id: s.id }} className="min-w-0 flex-1 truncate font-medium hover:underline">
                  {s.name}
                </Link>
                <span className="text-xs text-muted">{dyn(t, `cmdb.states.${s.state}`)}</span>
              </div>
              {s.causes.length > 0 && (
                <ul className="mt-1 space-y-0.5 pl-5">
                  {s.causes.map((c) => (
                    <Cause key={c.id} a={c} own={c.asset_id === s.id} />
                  ))}
                </ul>
              )}
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

/** Открытые оповещения мониторинга по объекту каталога. */
export function AssetAlerts({ id }: { id: string }) {
  const { t } = useTranslation();
  const q = useQuery({ queryKey: ["asset-alerts", id], queryFn: () => ok<AssetAlert[]>(api.GET("/api/helpdesk/assets/{id}/alerts", { params: { path: { id } } })) });
  if (q.error) return <ErrorBox error={q.error} />;
  if (!q.data || q.data.length === 0) return null;
  return (
    <Card className="mb-4 border-bad/40 p-4">
      <h2 className="mb-1 text-sm font-semibold text-bad">{t("cmdb.alerts", { n: q.data.length })}</h2>
      <ul className="space-y-1">
        {q.data.map((a) => (
          <Cause key={a.id} a={a} own />
        ))}
      </ul>
    </Card>
  );
}
