// Админка → Каталог AD: подключение, проверка контроллеров, синхронизация и журнал.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { api, ApiError, ok, type AdOverview, type DcCheckView, type DomainInput, type Summary } from "../../api/client";
import { Badge, Button, Card, ErrorBox, Field, Input, PageHeader, Select, Spinner } from "../../ui";
import { dyn } from "../helpdesk/helpdeskUtil";
import { formatDate } from "../../format";

const DEFAULT_FILTER = "(&(objectCategory=person)(objectClass=user))";

function emptyInput(): DomainInput {
  return {
    netbios_name: null,
    base_dn: "",
    controllers: [],
    tls_mode: "ldaps",
    bind_dn: "",
    bind_password: null,
    ca_cert_pem: null,
    user_filter: DEFAULT_FILTER,
    timeout_secs: 5,
    version: 0,
  };
}

function fromDomain(d: AdOverview["domain"]): DomainInput {
  if (!d) return emptyInput();
  return {
    netbios_name: d.netbios_name ?? null,
    base_dn: d.base_dn,
    controllers: d.controllers,
    tls_mode: d.tls_mode,
    bind_dn: d.bind_dn,
    bind_password: null,
    ca_cert_pem: null,
    user_filter: d.user_filter,
    timeout_secs: d.timeout_secs,
    version: d.version,
  };
}

export function AdPage() {
  const q = useQuery({ queryKey: ["admin-ad"], queryFn: () => ok<AdOverview>(api.GET("/api/admin/ad")) });
  if (q.isPending) return <Spinner />;
  if (q.error) return <ErrorBox error={q.error} onRetry={() => void q.refetch()} />;
  // Новая версия домена — форма с новыми значениями.
  return <AdView key={q.data.domain?.version ?? 0} data={q.data} />;
}

function AdView({ data }: { data: AdOverview }) {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const q = { data };
  const d = data.domain;
  const [form, setForm] = useState<DomainInput>(fromDomain(d));
  const [controllers, setControllers] = useState((d?.controllers ?? []).join("\n"));
  const [checks, setChecks] = useState<DcCheckView[] | null>(null);
  const [summary, setSummary] = useState<Summary | null>(null);

  const save = useMutation({
    meta: { toast: t("common.saved") },
    mutationFn: () =>
      ok(api.PUT("/api/admin/ad", { body: { ...form, controllers: controllers.split(/[\s,]+/).filter(Boolean) } })),
    onSuccess: () => {
      setForm((f) => ({ ...f, bind_password: null, ca_cert_pem: null }));
      void qc.invalidateQueries({ queryKey: ["admin-ad"] });
    },
  });
  const test = useMutation({ mutationFn: () => ok<DcCheckView[]>(api.POST("/api/admin/ad/test")), onSuccess: setChecks });
  const sync = useMutation({
    mutationFn: ({ kind, allow = false }: { kind: "full" | "incremental"; allow?: boolean }) =>
      ok<Summary>(api.POST("/api/admin/ad/sync", { body: { kind, allow_mass_disable: allow } })),
    onSuccess: (s) => {
      setSummary(s);
      void qc.invalidateQueries({ queryKey: ["admin-ad"] });
    },
    onError: (e, v) => {
      void qc.invalidateQueries({ queryKey: ["admin-ad"] });
      // Сервер не отключает много учёток сразу без подтверждения: возможно, сломан фильтр или права.
      if (e instanceof ApiError && e.code === "ad_sync_mass_disable" && window.confirm(`${e.message}\n\n${t("admin.ad.confirmMassDisable")}`)) {
        sync.mutate({ kind: v.kind, allow: true });
      }
    },
  });

  const submit = (e: FormEvent) => {
    e.preventDefault();
    save.mutate();
  };
  const h = q.data.health;

  return (
    <div className="mx-auto max-w-4xl space-y-6">
      <PageHeader title={t("admin.ad.title")}>
        <Badge tone={h.suspended ? "bad" : h.failures > 0 ? "accent" : "ok"}>
          {h.suspended ? t("admin.ad.suspended") : h.failures > 0 ? t("admin.ad.failing", { n: h.failures }) : t("admin.ad.healthy")}
        </Badge>
        <span className="text-sm text-muted">{t("admin.ad.accounts", { total: q.data.accounts, active: q.data.active_accounts })}</span>
      </PageHeader>

      <Card className="p-5">
        <form onSubmit={submit} className="grid gap-4 md:grid-cols-2">
          <Field label={t("admin.ad.baseDn")} hint={d ? `${t("admin.ad.dnsName")}: ${d.dns_name}` : undefined}>
            <Input value={form.base_dn} onChange={(e) => setForm({ ...form, base_dn: e.target.value })} placeholder="DC=corp,DC=example,DC=local" required />
          </Field>
          <Field label={t("admin.ad.netbios")}>
            <Input value={form.netbios_name ?? ""} onChange={(e) => setForm({ ...form, netbios_name: e.target.value || null })} placeholder="CORP" />
          </Field>
          <Field label={t("admin.ad.controllers")} hint={t("admin.ad.controllersHint")}>
            <textarea
              value={controllers}
              onChange={(e) => setControllers(e.target.value)}
              rows={3}
              required
              className="w-full rounded-lg border border-control bg-paper px-3 py-2 font-mono text-sm focus:border-accent"
            />
          </Field>
          <div className="space-y-4">
            <Field label={t("admin.ad.tls")}>
              <Select value={form.tls_mode} onChange={(e) => setForm({ ...form, tls_mode: e.target.value })}>
                <option value="ldaps">LDAPS (636)</option>
                <option value="starttls">StartTLS (389)</option>
              </Select>
            </Field>
            <Field label={t("admin.ad.timeout")}>
              <Input type="number" min={1} max={60} value={form.timeout_secs} onChange={(e) => setForm({ ...form, timeout_secs: Number(e.target.value) })} />
            </Field>
          </div>
          <Field label={t("admin.ad.bindDn")}>
            <Input value={form.bind_dn} onChange={(e) => setForm({ ...form, bind_dn: e.target.value })} placeholder="svc-pult-ldap@corp.example.local" required />
          </Field>
          <Field label={t("admin.ad.bindPassword")} hint={d ? t("admin.ad.passwordKept") : undefined}>
            <Input type="password" autoComplete="new-password" value={form.bind_password ?? ""} onChange={(e) => setForm({ ...form, bind_password: e.target.value || null })} required={!d} />
          </Field>
          <Field label={t("admin.ad.filter")}>
            <Input className="font-mono" value={form.user_filter} onChange={(e) => setForm({ ...form, user_filter: e.target.value })} />
          </Field>
          <Field label={t("admin.ad.ca")} hint={d?.has_ca_cert ? t("admin.ad.caKept") : t("admin.ad.caHint")}>
            <textarea
              value={form.ca_cert_pem ?? ""}
              onChange={(e) => setForm({ ...form, ca_cert_pem: e.target.value || null })}
              rows={3}
              placeholder="-----BEGIN CERTIFICATE-----"
              className="w-full rounded-lg border border-control bg-paper px-3 py-2 font-mono text-xs focus:border-accent"
            />
          </Field>
          <div className="flex flex-wrap items-center gap-2 md:col-span-2">
            <Button type="submit" variant="primary" disabled={save.isPending}>
              {t("common.save")}
            </Button>
            {d && (
              <Button onClick={() => test.mutate()} disabled={test.isPending}>
                {test.isPending ? t("common.loading") : t("admin.ad.test")}
              </Button>
            )}
          </div>
          {save.error && (
            <div className="md:col-span-2">
              <ErrorBox error={save.error} />
            </div>
          )}
        </form>
        {checks && (
          <ul className="mt-4 space-y-1 border-t border-line pt-4 text-sm">
            {checks.map((c) => (
              <li key={c.controller} className="flex gap-2">
                <Badge tone={c.ok ? "ok" : "bad"}>{c.ok ? t("admin.ad.dcOk") : t("admin.ad.dcFail")}</Badge>
                <span className="font-mono">{c.controller}</span>
                {c.hint && <div className="text-bad">{c.hint}</div>}
{c.error && <span className="text-bad">{c.error}</span>}
              </li>
            ))}
          </ul>
        )}
        {test.error && <ErrorBox error={test.error} />}
      </Card>

      {d && (
        <Card className="p-5">
          <div className="mb-4 flex flex-wrap items-center gap-2">
            <h2 className="font-semibold">{t("admin.ad.sync")}</h2>
            <Button className="ml-auto" onClick={() => sync.mutate({ kind: "incremental" })} disabled={sync.isPending}>
              {t("admin.ad.syncIncremental")}
            </Button>
            <Button variant="primary" onClick={() => sync.mutate({ kind: "full" })} disabled={sync.isPending}>
              {sync.isPending ? t("common.loading") : t("admin.ad.syncFull")}
            </Button>
          </div>
          {summary && (
            <p className="mb-3 text-sm text-ok">
              {t("admin.ad.summary", {
                seen: summary.seen,
                added: summary.added,
                disabled: summary.disabled,
                linked: summary.linked,
                created: summary.persons_created,
                candidates: summary.candidates,
              })}
            </p>
          )}
          {sync.error && <ErrorBox error={sync.error} />}
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead className="text-left text-xs text-muted">
                <tr>
                  <th className="py-1 pr-3 font-medium">{t("admin.ad.runStarted")}</th>
                  <th className="py-1 pr-3 font-medium">{t("admin.ad.runKind")}</th>
                  <th className="py-1 pr-3 font-medium">DC</th>
                  <th className="py-1 pr-3 font-medium">{t("admin.ad.runCounts")}</th>
                  <th className="py-1 font-medium">{t("admin.ad.runStatus")}</th>
                </tr>
              </thead>
              <tbody>
                {q.data.runs.map((r) => (
                  <tr key={r.id} className="border-t border-line">
                    <td className="py-1.5 pr-3 whitespace-nowrap">{formatDate(r.started_at, i18n.language)}</td>
                    <td className="py-1.5 pr-3">{t(r.kind === "full" ? "admin.ad.syncFull" : "admin.ad.syncIncremental")}</td>
                    <td className="py-1.5 pr-3 font-mono text-xs">{r.controller}</td>
                    <td className="py-1.5 pr-3 tabular-nums">
                      +{r.added} / ~{r.updated} / −{r.disabled}
                    </td>
                    <td className="py-1.5">
                      <Badge tone={r.status === "ok" ? "ok" : r.status === "error" ? "bad" : "muted"}>{dyn(t, `admin.ad.runState.${r.status}`)}</Badge>
                      {r.error && <span className="ml-2 text-xs text-bad">{r.error}</span>}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </Card>
      )}
    </div>
  );
}
