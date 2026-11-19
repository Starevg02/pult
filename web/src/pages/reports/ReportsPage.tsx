// Отчёты Helpdesk за период: сводка, разрезы, выгрузка в Excel и CSV.
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type Bucket, type Report, type Site } from "../../api/client";
import { dyn } from "../helpdesk/helpdeskUtil";
import { Card, ErrorBox, Field, Input, PageHeader, Select, Spinner } from "../../ui";

const iso = (d: Date) => d.toISOString().slice(0, 10);

function monthStart() {
  const d = new Date();
  return iso(new Date(d.getFullYear(), d.getMonth(), 1, 12));
}

function Stat({ label, value, hint }: { label: string; value: string | number; hint?: string }) {
  return (
    <Card className="p-3">
      <div className="text-xs text-muted">{label}</div>
      <div className="mt-1 text-2xl font-semibold tabular-nums">{value}</div>
      {hint && <div className="text-xs text-muted">{hint}</div>}
    </Card>
  );
}

function Table({ title, rows, label }: { title: string; rows: Bucket[]; label?: (name: string) => string }) {
  const { t } = useTranslation();
  if (rows.length === 0) return null;
  const max = Math.max(1, ...rows.map((r) => r.created));
  return (
    <Card className="overflow-x-auto p-3">
      <h2 className="mb-2 text-sm font-medium">{title}</h2>
      <table className="w-full text-sm">
        <thead className="text-left text-xs text-muted">
          <tr>
            <th className="py-1 pr-2 font-normal" />
            <th className="py-1 pr-2 text-right font-normal">{t("reports.created")}</th>
            <th className="py-1 pr-2 text-right font-normal">{t("reports.resolved")}</th>
            <th className="py-1 pr-2 text-right font-normal">{t("reports.avgResolve")}</th>
            <th className="py-1 text-right font-normal">{t("reports.work")}</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((r) => (
            <tr key={r.name} className="border-t border-line">
              <td className="relative py-1 pr-2">
                <span className="absolute inset-y-1 left-0 rounded bg-accent/10" style={{ width: `${(r.created / max) * 100}%` }} />
                <span className="relative">{(label ? label(r.name) : r.name) || "—"}</span>
              </td>
              <td className="py-1 pr-2 text-right tabular-nums">{r.created}</td>
              <td className="py-1 pr-2 text-right tabular-nums">{r.resolved}</td>
              <td className="py-1 pr-2 text-right tabular-nums">{r.avg_resolve_hours == null ? "—" : r.avg_resolve_hours.toFixed(1)}</td>
              <td className="py-1 text-right tabular-nums">{r.minutes ? (r.minutes / 60).toFixed(1) : ""}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </Card>
  );
}

const pct = (v: number | null | undefined) => (v == null ? "—" : `${Math.round(v)}%`);
const num = (v: number | null | undefined, digits = 0) => (v == null ? "—" : v.toFixed(digits));

export function ReportsPage() {
  const { t } = useTranslation();
  const [from, setFrom] = useState(monthStart);
  const [to, setTo] = useState(() => iso(new Date()));
  const [site, setSite] = useState("");
  const sites = useQuery({ queryKey: ["sites"], queryFn: () => ok<Site[]>(api.GET("/api/sites")) });
  const query = { from, to, site_id: site || undefined };
  const r = useQuery({ queryKey: ["report", from, to, site], queryFn: () => ok<Report>(api.GET("/api/helpdesk/reports", { params: { query } })) });
  const exportUrl = (format: string) => `/api/helpdesk/reports/export?${new URLSearchParams({ from, to, format, ...(site ? { site_id: site } : {}) })}`;
  const s = r.data?.summary;
  return (
    <div>
      <PageHeader title={t("reports.title")}>
        <a href={exportUrl("xlsx")} className="rounded-lg bg-ink px-3 py-1.5 text-sm font-medium text-paper">
          Excel
        </a>
        <a href={exportUrl("csv")} className="rounded-lg border border-line-strong px-3 py-1.5 text-sm">
          CSV
        </a>
      </PageHeader>
      <div className="mb-4 flex flex-wrap items-end gap-3">
        <Field label={t("reports.from")}>
          <Input type="date" value={from} onChange={(e) => setFrom(e.target.value)} />
        </Field>
        <Field label={t("reports.to")}>
          <Input type="date" value={to} onChange={(e) => setTo(e.target.value)} />
        </Field>
        <Field label={t("helpdesk.site")}>
          <Select value={site} onChange={(e) => setSite(e.target.value)}>
            <option value="">{t("helpdesk.allSites")}</option>
            {sites.data?.map((x) => (
              <option key={x.id} value={x.id}>
                {x.name}
              </option>
            ))}
          </Select>
        </Field>
      </div>
      {r.error ? (
        <ErrorBox error={r.error} />
      ) : !r.data || !s ? (
        <Spinner />
      ) : (
        <div className="space-y-4">
          <div className="grid grid-cols-2 gap-3 md:grid-cols-4 xl:grid-cols-6">
            <Stat label={t("reports.created")} value={s.created} />
            <Stat label={t("reports.resolved")} value={s.resolved} />
            <Stat label={t("reports.openNow")} value={s.open_now} hint={t("reports.unassignedNow", { n: s.unassigned_now })} />
            <Stat label={t("reports.overdueNow")} value={s.overdue_now} />
            <Stat label={t("reports.slaShare")} value={pct(s.sla_share)} hint={`${s.sla_met} / ${s.sla_total}`} />
            <Stat label={t("reports.rating")} value={pct(s.rating_share)} hint={`👍 ${s.rating_up} · 👎 ${s.rating_down}`} />
            <Stat label={t("reports.firstResponse")} value={num(s.avg_first_response_minutes)} hint={t("reports.minutesUnit")} />
            <Stat label={t("reports.avgResolve")} value={num(s.avg_resolve_hours, 1)} hint={t("reports.hoursUnit")} />
            <Stat label={t("reports.reopened")} value={pct(s.reopened_share)} hint={String(s.reopened)} />
            <Stat label={t("reports.firstContact")} value={pct(s.first_contact_share)} />
            <Stat label={t("reports.work")} value={(s.work_minutes / 60).toFixed(1)} hint={t("reports.hoursUnit")} />
            <Stat label={t("reports.aging")} value={`${s.open_older_1d} / ${s.open_older_3d} / ${s.open_older_7d}`} hint={t("reports.agingHint")} />
          </div>
          <div className="grid gap-4 lg:grid-cols-2">
            <Table title={t("reports.byAssignee")} rows={r.data.by_assignee} />
            <Table title={t("reports.byCategory")} rows={r.data.by_category} />
            <Table title={t("reports.bySite")} rows={r.data.by_site} />
            <Table title={t("reports.byDepartment")} rows={r.data.by_department} />
            <Table title={t("reports.byChannel")} rows={r.data.by_channel} label={(c) => dyn(t, `helpdesk.channel.${c}`)} />
            <Table title={t("reports.topRequesters")} rows={r.data.top_requesters} />
            <Table title={t("reports.load")} rows={r.data.load} />
          </div>
        </div>
      )}
    </div>
  );
}
