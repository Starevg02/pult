// Админка: телефония и веб-почта (19.7), безопасность (19.9), аудит (19.11), система (19.12).
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type ChangeEvent } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type AuditRow, type ImportRun, type OidcCheck, type OidcView, type SecuritySettings, type TelephonyView, type TlsStatus, type BackupStatus, type BackupPolicy, type PushSettings, type LocaleSettings, type OrgSettings, type RolesMatrix, type VerifyResult } from "../../api/client";
import { Badge, Button, Card, ErrorBox, Field, Input, PageHeader, Select, Spinner } from "../../ui";
import { formatDate } from "../../format";
import { StatusPage } from "../StatusPage";
import { LANGUAGES } from "../../i18n";
import { useMe, useOrg } from "../../session";
import { ModulesCard, OrgIdentityCard } from "../OrgWizard";
import { AssistCard } from "../helpdesk/assist";
import { NetboxCard } from "../assets/NetboxParts";

export function TelephonyPage() {
  const q = useQuery({ queryKey: ["telephony"], queryFn: () => ok<TelephonyView>(api.GET("/api/admin/telephony")) });
  if (q.error) return <ErrorBox error={q.error} />;
  if (q.isPending) return <Spinner />;
  return <TelephonyForm data={q.data} />;
}

function TelephonyForm({ data }: { data: TelephonyView }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [v, setV] = useState(data.settings);
  const [token, setToken] = useState("");
  const q = { data };
  const save = useMutation({
    meta: { toast: t("common.saved") },
    mutationFn: (clear: boolean) => ok(api.PUT("/api/admin/telephony", { body: { settings: v, token: clear ? "" : token || null } })),
    onSuccess: () => {
      setToken("");
      void qc.invalidateQueries({ queryKey: ["telephony"] });
      void qc.invalidateQueries({ queryKey: ["phonebook-config"] });
    },
  });
  return (
    <div className="mx-auto max-w-2xl">
      <PageHeader title={t("admin.telephony.title")} />
      <Card className="space-y-4 p-5">
        <Field label={t("admin.telephony.mailTemplate")} hint={t("admin.telephony.mailHint")}>
          <Input className="font-mono" value={v.mail_template} onChange={(e) => setV({ ...v, mail_template: e.target.value })} />
        </Field>
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={v.enabled} onChange={(e) => setV({ ...v, enabled: e.target.checked })} />
          {t("admin.telephony.enabled")}
        </label>
        <Field label={t("admin.telephony.provider")}>
          <Select value={v.provider} onChange={(e) => setV({ ...v, provider: e.target.value })}>
            <option value="ami">{t("admin.telephony.providers.ami")}</option>
            <option value="callgateway">{t("admin.telephony.providers.callgateway")}</option>
          </Select>
        </Field>
        {v.provider === "ami" ? (
          <>
            <p className="text-sm text-muted">{t("admin.telephony.amiHelp")}</p>
            <div className="grid gap-4 sm:grid-cols-[1fr_8rem]">
              <Field label={t("admin.telephony.amiHost")}>
                <Input className="font-mono" value={v.ami_host} onChange={(e) => setV({ ...v, ami_host: e.target.value.trim() })} placeholder="pbx.example.local" />
              </Field>
              <Field label={t("admin.telephony.amiPort")}>
                <Input className="font-mono" inputMode="numeric" value={String(v.ami_port)} onChange={(e) => setV({ ...v, ami_port: Number(e.target.value.replace(/\D/g, "")) || 0 })} />
              </Field>
            </div>
            <Field label={t("admin.telephony.amiUser")}>
              <Input className="font-mono" value={v.ami_user} onChange={(e) => setV({ ...v, ami_user: e.target.value.trim() })} placeholder="pult" />
            </Field>
            <Field label={t("admin.telephony.amiChannel")} hint={t("admin.telephony.amiChannelHint")}>
              <Input className="font-mono" value={v.ami_channel} onChange={(e) => setV({ ...v, ami_channel: e.target.value.trim() })} />
            </Field>
            <Field label={t("admin.telephony.amiContext")} hint={t("admin.telephony.amiContextHint")}>
              <Input className="font-mono" value={v.ami_context} onChange={(e) => setV({ ...v, ami_context: e.target.value.trim() })} />
            </Field>
          </>
        ) : (
          <Field label={t("admin.telephony.baseUrl")} hint={t("admin.telephony.baseUrlHint")}>
            <Input className="font-mono" value={v.base_url} onChange={(e) => setV({ ...v, base_url: e.target.value })} placeholder="https://callgateway.example.local:8443" />
          </Field>
        )}
        <Field label={v.provider === "ami" ? t("admin.telephony.amiSecret") : t("admin.telephony.token")} hint={q.data.has_token ? t("admin.telephony.tokenKept") : t("admin.telephony.tokenNone")}>
          <Input type="password" autoComplete="new-password" value={token} onChange={(e) => setToken(e.target.value)} />
        </Field>
        {save.error && <ErrorBox error={save.error} />}
        <div className="flex gap-2">
          <Button variant="primary" onClick={() => save.mutate(false)} disabled={save.isPending}>
            {t("common.save")}
          </Button>
          {q.data.has_token && <Button onClick={() => save.mutate(true)}>{t("admin.telephony.clearToken")}</Button>}
        </div>
      </Card>
    </div>
  );
}

const NUMBERS = [
  "session_idle_hours",
  "session_absolute_days",
  "login_max_failures_per_login",
  "login_max_failures_per_ip",
  "login_window_minutes",
  "ad_outage_grace_minutes",
] as const;

export function SecurityPage() {
  const q = useQuery({ queryKey: ["security"], queryFn: () => ok<SecuritySettings>(api.GET("/api/admin/security")) });
  // Вход через провайдера настраивает только суперадминистратор: провайдер решает, под чьей учёткой входят.
  const isSuper = useMe().data?.is_superadmin === true;
  if (q.error) return <ErrorBox error={q.error} />;
  if (q.isPending) return <Spinner />;
  return (
    <>
      <SecurityForm data={q.data} />
      {isSuper && <OidcCard />}
    </>
  );
}

const OIDC_CLAIMS = ["preferred_username", "upn", "email"] as const;

function OidcCard() {
  const q = useQuery({ queryKey: ["oidc"], queryFn: () => ok<OidcView>(api.GET("/api/admin/oidc")) });
  if (q.error) return <ErrorBox error={q.error} />;
  if (q.isPending) return <Spinner />;
  return <OidcForm data={q.data} />;
}

/** Вход через провайдера OpenID Connect: издатель, клиент, секрет (не показывается), поле с логином. */
function OidcForm({ data }: { data: OidcView }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [v, setV] = useState(data.settings);
  const [secret, setSecret] = useState("");
  const [dropSecret, setDropSecret] = useState(false);
  const callback = `${window.location.origin}/api/auth/oidc/callback`;
  const save = useMutation({
    meta: { toast: t("common.saved") },
    mutationFn: () => ok(api.PUT("/api/admin/oidc", { body: { settings: v, client_secret: dropSecret ? "" : secret || null } })),
    onSuccess: () => {
      setSecret("");
      setDropSecret(false);
      void qc.invalidateQueries({ queryKey: ["oidc"] });
      void qc.invalidateQueries({ queryKey: ["setup"] });
    },
  });
  const test = useMutation({ mutationFn: () => ok<OidcCheck>(api.POST("/api/admin/oidc/test")) });
  const reset = useMutation({
    meta: { toast: t("common.saved") },
    mutationFn: () => ok(api.DELETE("/api/admin/oidc/identities")),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["oidc"] }),
  });
  return (
    <div className="mx-auto mt-4 max-w-2xl">
      <Card className="grid gap-4 p-5 sm:grid-cols-2">
        <div className="sm:col-span-2">
          <h2 className="font-semibold">{t("admin.oidc.title")}</h2>
          <p className="mt-1 text-xs text-muted">{t("admin.oidc.hint")}</p>
        </div>
        <label className="inline-flex items-center gap-2 text-sm font-medium sm:col-span-2">
          <input type="checkbox" checked={v.enabled} onChange={(e) => setV({ ...v, enabled: e.target.checked })} />
          {t("admin.oidc.enabled")}
        </label>
        <div className="sm:col-span-2">
          <Field label={t("admin.oidc.issuer")} hint={t("admin.oidc.issuerHint")}>
            <Input value={v.issuer} onChange={(e) => setV({ ...v, issuer: e.target.value })} placeholder="https://sso.example.local/realms/corp" />
          </Field>
        </div>
        <Field label={t("admin.oidc.clientId")}>
          <Input value={v.client_id} onChange={(e) => setV({ ...v, client_id: e.target.value })} />
        </Field>
        <Field label={t("admin.oidc.secret")} hint={data.has_secret ? t("admin.ad.passwordKept") : t("admin.oidc.publicClient")}>
          <Input type="password" autoComplete="new-password" value={secret} disabled={dropSecret} onChange={(e) => setSecret(e.target.value)} />
          {data.has_secret && (
            <label className="mt-1 inline-flex items-center gap-2 text-xs text-muted">
              <input type="checkbox" checked={dropSecret} onChange={(e) => setDropSecret(e.target.checked)} />
              {t("admin.oidc.dropSecret")}
            </label>
          )}
        </Field>
        <div className="sm:col-span-2">
          <Field label={t("admin.oidc.redirect")} hint={t("admin.oidc.redirectHint", { url: callback })}>
            <div className="flex gap-2">
              <Input value={v.redirect_uri} onChange={(e) => setV({ ...v, redirect_uri: e.target.value })} placeholder={callback} />
              <Button type="button" onClick={() => setV({ ...v, redirect_uri: callback })}>
                {t("admin.oidc.useThis")}
              </Button>
            </div>
          </Field>
        </div>
        <Field label={t("admin.oidc.claim")} hint={t("admin.oidc.claimHint")}>
          <Select
            value={v.username_claim}
            onChange={(e) => setV({ ...v, username_claim: OIDC_CLAIMS.find((c) => c === e.target.value) ?? "preferred_username" })}
          >
            {OIDC_CLAIMS.map((c) => (
              <option key={c} value={c}>
                {t(`admin.oidc.claims.${c}`)}
              </option>
            ))}
          </Select>
        </Field>
        <Field label={t("admin.oidc.buttonLabel")}>
          <Input value={v.button_label} maxLength={60} onChange={(e) => setV({ ...v, button_label: e.target.value })} placeholder={t("login.oidc.button")} />
        </Field>
        <div className="sm:col-span-2">
          <label className="inline-flex items-center gap-2 text-sm font-medium">
            <input type="checkbox" checked={v.allow_external} onChange={(e) => setV({ ...v, allow_external: e.target.checked })} />
            {t("admin.oidc.allowExternal")}
          </label>
          <p className="mt-1 text-xs text-muted">{t("admin.oidc.allowExternalHint")}</p>
        </div>
        <div className="flex flex-wrap items-center gap-2 sm:col-span-2">
          <Button variant="primary" onClick={() => save.mutate()} disabled={save.isPending}>
            {t("common.save")}
          </Button>
          <Button onClick={() => test.mutate()} disabled={test.isPending || !data.settings.issuer}>
            {t("admin.oidc.test")}
          </Button>
          {test.data && (
            <span className={`text-sm ${test.data.ok ? "text-ok" : "text-bad"}`}>
              {test.data.ok ? t("admin.oidc.testOk", { count: test.data.keys }) : (test.data.error ?? t("admin.oidc.noKeys"))}
            </span>
          )}
        </div>
        <div className="flex flex-wrap items-center gap-2 border-t border-line pt-3 text-sm sm:col-span-2">
          <span className="text-muted">{t("admin.oidc.identities", { count: data.identities })}</span>
          <Button
            variant="ghost"
            className="min-h-7 px-2 text-xs"
            disabled={data.identities === 0 || reset.isPending}
            onClick={() => {
              if (window.confirm(t("admin.oidc.resetConfirm"))) reset.mutate();
            }}
          >
            {t("admin.oidc.reset")}
          </Button>
          <p className="w-full text-xs text-muted">{t("admin.oidc.identitiesHint")}</p>
        </div>
        {(save.error || test.error || reset.error) && (
          <div className="sm:col-span-2">
            <ErrorBox error={save.error ?? test.error ?? reset.error} />
          </div>
        )}
      </Card>
    </div>
  );
}

function SecurityForm({ data }: { data: SecuritySettings }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [v, setV] = useState(data);
  const [nets, setNets] = useState(data.internal_networks.join("\n"));
  // Список ролей — для галочек «код обязателен»; без права на роли настройка просто сохраняется как есть.
  const roles = useQuery({ queryKey: ["roles"], queryFn: () => ok<RolesMatrix>(api.GET("/api/admin/roles")), retry: false });
  const required = new Set(v.totp_required_roles);
  const toggleRole = (code: string, on: boolean) => {
    const next = new Set(required);
    if (on) next.add(code);
    else next.delete(code);
    setV({ ...v, totp_required_roles: [...next].sort() });
  };
  const save = useMutation({
    meta: { toast: t("common.saved") },
    mutationFn: () =>
      ok(api.PUT("/api/admin/security", { body: { ...v, internal_networks: nets.split(/[\s,;]+/).filter(Boolean) } })),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["security"] }),
  });
  return (
    <div className="mx-auto max-w-2xl">
      <PageHeader title={t("admin.security.title")} />
      <Card className="grid gap-4 p-5 sm:grid-cols-2">
        {NUMBERS.map((k) => (
          <Field key={k} label={t(`admin.security.${k}`)}>
            <Input type="number" min={1} value={v[k]} onChange={(e) => setV({ ...v, [k]: Number(e.target.value) })} />
          </Field>
        ))}
        <Field label={t("admin.security.ad_outage_policy")} hint={t("admin.security.policyHint")}>
          <Select value={v.ad_outage_policy} onChange={(e) => setV({ ...v, ad_outage_policy: e.target.value === "grace" ? "grace" : "block" })}>
            <option value="block">{t("admin.security.policy.block")}</option>
            <option value="grace">{t("admin.security.policy.grace")}</option>
          </Select>
        </Field>
        <div className="sm:col-span-2">
          <Field label={t("admin.security.internal_networks")} hint={t("admin.security.internalHint")}>
            <textarea
              value={nets}
              onChange={(e) => setNets(e.target.value)}
              rows={4}
              className="w-full rounded-lg border border-control bg-paper px-3 py-2 font-mono text-sm focus:border-accent"
            />
          </Field>
        </div>
        <div className="sm:col-span-2">
          <label className="inline-flex items-center gap-2 text-sm font-medium">
            <input type="checkbox" checked={v.kerberos_login} onChange={(e) => setV({ ...v, kerberos_login: e.target.checked })} />
            {t("admin.security.kerberos_login")}
          </label>
          <p className="mt-1 text-xs text-muted">{t("admin.security.kerberosHint")}</p>
        </div>
        <div className="sm:col-span-2">
          <label className="inline-flex items-center gap-2 text-sm font-medium">
            <input type="checkbox" checked={v.ad_password_change} onChange={(e) => setV({ ...v, ad_password_change: e.target.checked })} />
            {t("admin.security.ad_password_change")}
          </label>
          <p className="mt-1 text-xs text-muted">{t("admin.security.adPasswordHint")}</p>
        </div>
        {roles.data && (
          <div className="sm:col-span-2">
            <Field label={t("admin.security.totp_required_roles")} hint={t("admin.security.totpRolesHint")}>
              <div className="flex flex-wrap gap-x-5 gap-y-2 text-sm">
                {roles.data.roles.map((r) => (
                  <label key={r.code} className="inline-flex items-center gap-2">
                    <input type="checkbox" checked={required.has(r.code)} onChange={(e) => toggleRole(r.code, e.target.checked)} />
                    {r.name}
                  </label>
                ))}
              </div>
            </Field>
          </div>
        )}
        <div className="sm:col-span-2">
          {save.error && <ErrorBox error={save.error} />}
          <Button variant="primary" onClick={() => save.mutate()} disabled={save.isPending}>
            {t("common.save")}
          </Button>
        </div>
      </Card>
    </div>
  );
}

type AuditFilter = { action: string; object_type: string; object_id: string; from: string; to: string };

function toQuery(f: AuditFilter, before?: number) {
  const iso = (d: string) => (d ? new Date(d).toISOString() : undefined);
  return {
    action: f.action || undefined,
    object_type: f.object_type || undefined,
    object_id: f.object_id || undefined,
    from: iso(f.from),
    to: iso(f.to),
    before_id: before,
  };
}

export function AuditPage() {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const [f, setF] = useState<AuditFilter>({ action: "", object_type: "", object_id: "", from: "", to: "" });
  const [applied, setApplied] = useState(f);
  const q = useQuery({ queryKey: ["audit", applied], queryFn: () => ok<AuditRow[]>(api.GET("/api/admin/audit", { params: { query: { ...toQuery(applied), limit: 200 } } })) });
  const verify = useQuery({ queryKey: ["audit-verify"], queryFn: () => ok<VerifyResult>(api.GET("/api/admin/audit/verify")) });
  const run = useMutation({
    mutationFn: () => ok<VerifyResult>(api.POST("/api/admin/audit/verify")),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["audit-verify"] }),
  });
  const csv = () => {
    const p = new URLSearchParams();
    for (const [k, v] of Object.entries(toQuery(applied))) if (v !== undefined) p.set(k, String(v));
    window.location.href = `/api/admin/audit.csv?${p.toString()}`;
  };
  const vr = verify.data;
  return (
    <div className="mx-auto max-w-6xl space-y-4">
      <PageHeader title={t("admin.audit.title")}>
        {vr?.checked_at && (
          <Badge tone={vr.broken_at ? "bad" : "ok"}>
            {vr.broken_at ? t("admin.audit.broken", { id: vr.broken_at }) : t("admin.audit.intact", { total: vr.total, at: formatDate(vr.checked_at, i18n.language) })}
          </Badge>
        )}
        <Button onClick={() => run.mutate()} disabled={run.isPending}>
          {t("admin.audit.verify")}
        </Button>
        <Button onClick={csv}>{t("admin.audit.csv")}</Button>
      </PageHeader>
      <Card className="p-4">
        <form
          className="grid gap-3 sm:grid-cols-3 lg:grid-cols-6"
          onSubmit={(e) => {
            e.preventDefault();
            setApplied(f);
          }}
        >
          {(["action", "object_type", "object_id"] as const).map((k) => (
            <Field key={k} label={t(`admin.audit.${k}`)}>
              <Input value={f[k]} onChange={(e) => setF({ ...f, [k]: e.target.value })} />
            </Field>
          ))}
          <Field label={t("admin.audit.from")}>
            <Input type="datetime-local" value={f.from} onChange={(e) => setF({ ...f, from: e.target.value })} />
          </Field>
          <Field label={t("admin.audit.to")}>
            <Input type="datetime-local" value={f.to} onChange={(e) => setF({ ...f, to: e.target.value })} />
          </Field>
          <div className="flex items-end">
            <Button type="submit" variant="primary" className="w-full">
              {t("common.search")}
            </Button>
          </div>
        </form>
      </Card>
      {(q.error ?? run.error) && <ErrorBox error={q.error ?? run.error} />}
      {q.isPending ? (
        <Spinner />
      ) : (
        <Card className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead className="border-b border-line text-left text-xs text-muted">
              <tr>
                <th className="px-3 py-2 font-medium">#</th>
                <th className="px-3 py-2 font-medium">{t("admin.audit.ts")}</th>
                <th className="px-3 py-2 font-medium">{t("admin.audit.actor")}</th>
                <th className="px-3 py-2 font-medium">{t("admin.audit.action")}</th>
                <th className="px-3 py-2 font-medium">{t("admin.audit.object")}</th>
                <th className="px-3 py-2 font-medium">{t("admin.audit.details")}</th>
              </tr>
            </thead>
            <tbody>
              {q.data?.map((r) => (
                <tr key={r.id} className="border-b border-line align-top last:border-0">
                  <td className="px-3 py-1.5 font-mono text-xs text-muted">{r.id}</td>
                  <td className="px-3 py-1.5 whitespace-nowrap">{formatDate(r.ts, i18n.language)}</td>
                  <td className="px-3 py-1.5">
                    {r.actor ?? <span className="text-muted">{t(`admin.audit.source.${r.source === "job" || r.source === "cli" || r.source === "legacy_phonebook" ? r.source : "system"}`)}</span>}
                    {r.ip && <div className="font-mono text-xs text-muted">{r.ip}</div>}
                  </td>
                  <td className="px-3 py-1.5 font-mono text-xs">{r.action}</td>
                  <td className="px-3 py-1.5 font-mono text-xs">{[r.object_type, r.object_id].filter(Boolean).join(":")}</td>
                  <td className="max-w-md truncate px-3 py-1.5 font-mono text-xs text-muted" title={JSON.stringify(r.details)}>
                    {JSON.stringify(r.details)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </Card>
      )}
    </div>
  );
}

function BackupPolicyForm({ policy }: { policy: BackupPolicy }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [keep, setKeep] = useState(String(policy.keep));
  const [verify, setVerify] = useState(policy.verify);
  const n = Number(keep);
  const valid = Number.isInteger(n) && n >= 1 && n <= 365;
  const save = useMutation({
    meta: { toast: t("common.saved") },
    mutationFn: () => ok<BackupPolicy>(api.PUT("/api/admin/backup/policy", { body: { keep: n, verify } })),
    onSuccess: (p) => qc.setQueryData(["backup-policy"], p),
  });
  const changed = n !== policy.keep || verify !== policy.verify;
  return (
    <div className="space-y-2 border-t border-line pt-3">
      <Field label={t("admin.system.backupKeep")} hint={t("admin.system.backupKeepHint")}>
        <Input type="number" min={1} max={365} value={keep} onChange={(e) => setKeep(e.target.value)} className="w-28" />
      </Field>
      <label className="flex items-center gap-2">
        <input type="checkbox" checked={verify} onChange={(e) => setVerify(e.target.checked)} />
        {t("admin.system.backupVerify")}
      </label>
      {save.error && <ErrorBox error={save.error} />}
      <Button disabled={!valid || !changed || save.isPending} onClick={() => save.mutate()}>
        {t("common.save")}
      </Button>
    </div>
  );
}

/** Уведомления на устройства (Web Push): серверу нужен выход к службам push браузеров. */
function PushCard() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["push-settings"], queryFn: () => ok<PushSettings>(api.GET("/api/admin/push")), retry: false });
  const [v, setV] = useState<PushSettings | null>(null);
  const cur = v ?? q.data;
  const save = useMutation({
    meta: { toast: t("common.saved") },
    mutationFn: (b: PushSettings) => ok<PushSettings>(api.PUT("/api/admin/push", { body: b })),
    onSuccess: (s) => {
      setV(null);
      qc.setQueryData(["push-settings"], s);
      void qc.invalidateQueries({ queryKey: ["my-push"] });
    },
  });
  if (!cur) return null;
  return (
    <div className="mx-auto max-w-xl">
      <h2 className="mb-2 font-semibold">{t("admin.system.push")}</h2>
      <Card className="space-y-2 p-4 text-sm">
        <p className="text-muted">{t("admin.system.pushHint")}</p>
        <label className="flex items-center gap-2">
          <input type="checkbox" checked={cur.enabled} onChange={(e) => setV({ ...cur, enabled: e.target.checked })} />
          {t("admin.system.pushEnabled")}
        </label>
        <label className="flex items-center gap-2">
          <input type="checkbox" checked={cur.show_text} onChange={(e) => setV({ ...cur, show_text: e.target.checked })} />
          {t("admin.system.pushShowText")}
        </label>
        <Field label={t("admin.system.pushContact")} hint={t("admin.system.pushContactHint")}>
          <Input value={cur.contact} onChange={(e) => setV({ ...cur, contact: e.target.value })} placeholder="mailto:it@example.local" />
        </Field>
        {save.error && <ErrorBox error={save.error} />}
        <Button disabled={!v || save.isPending} onClick={() => v && save.mutate(v)}>
          {t("common.save")}
        </Button>
      </Card>
    </div>
  );
}

function BackupCard() {
  const { t, i18n } = useTranslation();
  const q = useQuery({ queryKey: ["backup"], queryFn: () => ok<BackupStatus>(api.GET("/api/admin/backup")), retry: false });
  const policy = useQuery({ queryKey: ["backup-policy"], queryFn: () => ok<BackupPolicy>(api.GET("/api/admin/backup/policy")), retry: false });
  if (!q.data) return null;
  const b = q.data;
  return (
    <div className="mx-auto max-w-xl">
      <h2 className="mb-2 font-semibold">{t("admin.system.backup")}</h2>
      <Card className="space-y-1 p-4 text-sm">
        {!b.last_at ? (
          <p className="text-muted">{t("admin.system.backupNone")}</p>
        ) : (
          <>
            <div className="flex flex-wrap gap-2">
              <Badge tone={b.ok ? "ok" : "bad"}>{b.ok ? t("admin.system.backupOk") : t("admin.system.backupFailed")}</Badge>
              {b.ok && <Badge tone={b.verified ? "ok" : "muted"}>{b.verified ? t("admin.system.backupVerified", { n: b.objects ?? 0 }) : t("admin.system.backupNotVerified")}</Badge>}
            </div>
            <div className="text-muted">{formatDate(b.last_at, i18n.language)}</div>
            {b.path && <div className="font-mono text-xs break-all">{b.path} · {Math.round(b.size_bytes / 1024 / 1024)} MB</div>}
            {b.error && <div className="text-bad">{b.error}</div>}
          </>
        )}
        {policy.data && <BackupPolicyForm key={`${policy.data.keep}-${String(policy.data.verify)}`} policy={policy.data} />}
      </Card>
    </div>
  );
}

/** Язык установки: для новых пользователей, писем на адреса без учётки и фоновых текстов. */
function LocaleCard() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["locale"], queryFn: () => ok<LocaleSettings>(api.GET("/api/admin/locale")), retry: false });
  const save = useMutation({
    meta: { toast: t("common.saved") },
    mutationFn: (default_locale: string) => ok<LocaleSettings>(api.PUT("/api/admin/locale", { body: { default_locale } })),
    onSuccess: (s) => qc.setQueryData(["locale"], s),
  });
  if (!q.data) return null;
  return (
    <div className="mx-auto max-w-xl">
      <h2 className="mb-2 font-semibold">{t("admin.system.locale")}</h2>
      <Card className="space-y-2 p-4 text-sm">
        <Field label={t("admin.system.localeDefault")} hint={t("admin.system.localeHint")}>
          <Select value={q.data.default_locale} disabled={save.isPending} onChange={(e) => save.mutate(e.target.value)}>
            {LANGUAGES.map(([code, name]) => (
              <option key={code} value={code}>
                {name}
              </option>
            ))}
          </Select>
        </Field>
        {save.error && <ErrorBox error={save.error} />}
      </Card>
    </div>
  );
}

function TlsCard() {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["tls"], queryFn: () => ok<TlsStatus>(api.GET("/api/admin/tls")), retry: false });
  const [cert, setCert] = useState("");
  const [key, setKey] = useState("");
  const save = useMutation({
    meta: { toast: t("common.saved") },
    mutationFn: () => ok<TlsStatus>(api.PUT("/api/admin/tls", { body: { cert_pem: cert, key_pem: key } })),
    onSuccess: (s) => {
      setCert("");
      setKey("");
      qc.setQueryData(["tls"], s);
    },
    onError: () => setKey(""),
  });
  if (!q.data) return null;
  const info = q.data.info;
  const area = "w-full rounded-lg border border-control bg-paper px-3 py-2 font-mono text-xs focus:border-accent";
  const load = (set: (v: string) => void) => (e: ChangeEvent<HTMLInputElement>) => {
    void e.target.files?.[0]?.text().then(set);
  };
  return (
    <div className="mx-auto max-w-xl">
      <h2 className="mb-2 font-semibold">{t("admin.system.tls")}</h2>
      <Card className="space-y-3 p-4 text-sm">
        {!q.data.managed && <p className="text-muted">{t("admin.system.tlsExternal")}</p>}
        {info && (
          <>
            <div className="flex flex-wrap items-center gap-2">
              <span className="font-mono text-xs">{info.names.join(", ") || info.subject}</span>
              {info.self_signed && <Badge tone="bad">{t("admin.system.tlsSelfSigned")}</Badge>}
            </div>
            <div className="text-muted">{t("admin.system.tlsUntil", { date: formatDate(info.not_after, i18n.language) })}</div>
            <Field label={t("admin.system.tlsCert")} hint={t("admin.system.tlsCertHint")}>
              <input type="file" accept=".pem,.crt,.cer" onChange={load(setCert)} className="mb-1 text-xs" />
              <textarea rows={3} className={area} placeholder="-----BEGIN CERTIFICATE-----" value={cert} onChange={(e) => setCert(e.target.value)} />
            </Field>
            <Field label={t("admin.system.tlsKey")}>
              <input type="file" accept=".pem,.key" onChange={load(setKey)} className="mb-1 text-xs" />
              <textarea rows={3} className={area} placeholder="PEM" value={key} onChange={(e) => setKey(e.target.value)} />
            </Field>
            {save.error && <ErrorBox error={save.error} />}
            <Button variant="primary" disabled={save.isPending || !cert.trim() || !key.trim()} onClick={() => save.mutate()}>
              {t("admin.system.tlsSave")}
            </Button>
          </>
        )}
      </Card>
    </div>
  );
}

/** Организация: название, логотип, модули; мастер настройки можно пройти заново. */
function OrgCard() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const org = useOrg().data;
  const rerun = useMutation({
    mutationFn: (o: OrgSettings) => ok<OrgSettings>(api.PUT("/api/admin/org", { body: { name: o.name, modules: o.modules, setup_done: false } })),
    onSuccess: (o) => qc.setQueryData(["org"], o),
  });
  if (!org) return null;
  return (
    <div className="mx-auto max-w-xl space-y-3">
      <h2 className="font-semibold">{t("org.section")}</h2>
      <OrgIdentityCard org={org} withLanguage={false} />
      <ModulesCard org={org} />
      {rerun.error && <ErrorBox error={rerun.error} />}
      <Button variant="ghost" disabled={rerun.isPending} onClick={() => rerun.mutate(org)}>
        {t("org.rerun")}
      </Button>
    </div>
  );
}

export function SystemPage() {
  const { t, i18n } = useTranslation();
  // Помощника настраивает только суперадминистратор: модели уходят тексты заявок.
  const isSuper = useMe().data?.is_superadmin === true;
  const imports = useQuery({ queryKey: ["imports"], queryFn: () => ok<ImportRun[]>(api.GET("/api/admin/phonebook/imports")), retry: false });
  return (
    <div className="space-y-6">
      <StatusPage />
      <OrgCard />
      <LocaleCard />
      {isSuper && <AssistCard />}
      {isSuper && <NetboxCard />}
      <TlsCard />
      <BackupCard />
      <PushCard />
      {imports.data && (
        <div className="mx-auto max-w-xl">
          <h2 className="mb-2 font-semibold">{t("admin.system.imports")}</h2>
          <Card>
            <ul className="divide-y divide-line text-sm">
              {imports.data.length === 0 && <li className="px-4 py-3 text-muted">{t("admin.system.noImports")}</li>}
              {imports.data.map((r) => (
                <li key={r.id} className="px-4 py-2">
                  <div>
                    {formatDate(r.started_at, i18n.language)} · <span className="font-mono text-xs">{r.source}</span>
                  </div>
                  <div className="truncate font-mono text-xs text-muted" title={JSON.stringify(r.counts)}>
                    {JSON.stringify((r.counts as { reconciliation?: unknown }).reconciliation ?? r.counts)}
                  </div>
                </li>
              ))}
            </ul>
          </Card>
        </div>
      )}
    </div>
  );
}
