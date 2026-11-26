// Админка → Удалённая поддержка: токены регистрации агентов, политики ПК,
// пакеты агента и helper, ПК с агентом, настройки.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import {
  api,
  ApiError,
  csrfToken,
  ok,
  type AgentPackageRow,
  type CreatedEnrollmentToken,
  type DeviceRow,
  type EnrollmentTokenRow,
  type PolicyImportResult,
  type RemotePolicyRow,
  type RemoteSettings,
} from "../../api/client";
import { formatDate } from "../../format";
import { Badge, Button, Card, CopyButton, Empty, ErrorBox, Field, Input, PageHeader, Select, Spinner } from "../../ui";
import { dyn } from "../helpdesk/helpdeskUtil";
import { Online } from "../remote/RemotePages";

function Section({ title, hint, children }: { title: string; hint?: string; children: React.ReactNode }) {
  return (
    <Card className="p-4">
      <h2 className="text-sm font-semibold">{title}</h2>
      {hint && <p className="mb-3 mt-1 text-xs text-muted">{hint}</p>}
      {children}
    </Card>
  );
}

function SettingsSection() {
  const q = useQuery({ queryKey: ["remote-settings"], queryFn: () => ok<RemoteSettings>(api.GET("/api/admin/remote/settings")) });
  if (!q.data) return q.error ? <ErrorBox error={q.error} /> : <Spinner />;
  // Без key: фоновое обновление запроса не сбрасывает несохранённый ввод.
  return <SettingsForm initial={q.data} />;
}

function SettingsForm({ initial }: { initial: RemoteSettings }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [v, setV] = useState<RemoteSettings>(initial);
  const save = useMutation({
    meta: { toast: t("common.saved") },
    mutationFn: (s: RemoteSettings) => ok(api.PUT("/api/admin/remote/settings", { body: s })),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["remote-settings"] }),
  });
  const num = (k: "online_minutes" | "report_interval_secs") => (e: React.ChangeEvent<HTMLInputElement>) => setV({ ...v, [k]: Number(e.target.value) });
  return (
    <Section title={t("remoteAdmin.settings.title")}>
      <div className="space-y-3">
        <label className="flex items-start gap-2 text-sm">
          <input type="checkbox" className="mt-1" checked={v.require_dns_match} onChange={(e) => setV({ ...v, require_dns_match: e.target.checked })} />
          <span>
            {t("remoteAdmin.settings.dns")}
            <span className="block text-xs text-muted">{t("remoteAdmin.settings.dnsHint")}</span>
            {!v.require_dns_match && <span className="mt-1 block text-xs text-bad">{t("remoteAdmin.settings.dnsOff")}</span>}
          </span>
        </label>
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label={t("remoteAdmin.settings.online")}>
            <Input type="number" min={1} max={60} value={v.online_minutes} onChange={num("online_minutes")} />
          </Field>
          <Field label={t("remoteAdmin.settings.interval")}>
            <Input type="number" min={15} max={3600} value={v.report_interval_secs} onChange={num("report_interval_secs")} />
          </Field>
        </div>
        {save.error && <ErrorBox error={save.error} />}
        <Button variant="primary" disabled={save.isPending} onClick={() => save.mutate(v)}>
          {t("common.save")}
        </Button>
      </div>
    </Section>
  );
}

function TokensSection() {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["remote-tokens"], queryFn: () => ok<EnrollmentTokenRow[]>(api.GET("/api/admin/remote/tokens")) });
  const [name, setName] = useState("");
  const [days, setDays] = useState(30);
  const [created, setCreated] = useState<CreatedEnrollmentToken | null>(null);
  const create = useMutation({
    mutationFn: () => ok<CreatedEnrollmentToken>(api.POST("/api/admin/remote/tokens", { body: { name: name.trim(), days } })),
    onSuccess: (r) => {
      setCreated(r);
      setName("");
      void qc.invalidateQueries({ queryKey: ["remote-tokens"] });
    },
  });
  const revoke = useMutation({
    mutationFn: (id: string) => ok(api.POST("/api/admin/remote/tokens/{id}/revoke", { params: { path: { id } } })),
    onSettled: () => void qc.invalidateQueries({ queryKey: ["remote-tokens"] }),
  });
  const portal = `${window.location.origin}/`;
  return (
    <Section title={t("remoteAdmin.tokens.title")} hint={t("remoteAdmin.tokens.hint")}>
      <div className="flex flex-wrap items-end gap-2">
        <Field label={t("remoteAdmin.tokens.name")}>
          <Input value={name} onChange={(e) => setName(e.target.value)} placeholder={t("remoteAdmin.tokens.namePlaceholder")} />
        </Field>
        <Field label={t("remoteAdmin.tokens.days")}>
          <Input type="number" min={1} max={365} className="w-24" value={days} onChange={(e) => setDays(Number(e.target.value))} />
        </Field>
        <Button variant="primary" disabled={!name.trim() || create.isPending} onClick={() => create.mutate()}>
          {t("remoteAdmin.tokens.create")}
        </Button>
      </div>
      {create.error && <ErrorBox error={create.error} />}
      {created && (
        <div className="mt-3 space-y-2 rounded-lg border border-accent/40 bg-accentsoft/40 p-3 text-sm">
          <div className="font-medium">{t("remoteAdmin.tokens.once")}</div>
          <div className="flex items-center gap-1 font-mono text-xs break-all">
            {created.token} <CopyButton value={created.token} label={t("common.copy")} />
          </div>
          <div className="text-xs text-muted">{t("remoteAdmin.tokens.msi")}</div>
          <div className="flex items-center gap-1 font-mono text-xs break-all">
            {`msiexec /i pult-agent.msi /qn PORTALURL=${portal} ENROLLTOKEN=${created.token}`}
            <CopyButton value={`msiexec /i pult-agent.msi /qn PORTALURL=${portal} ENROLLTOKEN=${created.token}`} label={t("common.copy")} />
          </div>
          <Button variant="ghost" onClick={() => setCreated(null)}>
            {t("common.close")}
          </Button>
        </div>
      )}
      {q.data && q.data.length > 0 && (
        <ul className="mt-3 divide-y divide-line text-sm">
          {q.data.map((r) => {
            const dead = !!r.revoked_at || new Date(r.expires_at) < new Date();
            return (
              <li key={r.id} className={`flex flex-wrap items-center gap-2 py-2 ${dead ? "opacity-60" : ""}`}>
                <span className="font-medium">{r.name}</span>
                <span className="font-mono text-xs text-muted">{r.prefix}…</span>
                {r.revoked_at ? <Badge tone="bad">{t("remoteAdmin.tokens.revoked")}</Badge> : dead && <Badge>{t("remoteAdmin.tokens.expired")}</Badge>}
                <span className="text-xs text-muted">
                  {t("remoteAdmin.tokens.until", { d: formatDate(r.expires_at, i18n.language) })} · {t("remoteAdmin.tokens.uses", { n: r.uses })} · {r.created_by}
                </span>
                {!dead && (
                  <Button variant="danger" className="ml-auto" disabled={revoke.isPending} onClick={() => window.confirm(t("remoteAdmin.tokens.revokeConfirm", { name: r.name })) && revoke.mutate(r.id)}>
                    {t("remoteAdmin.tokens.revoke")}
                  </Button>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </Section>
  );
}

function PoliciesSection() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["remote-policies"], queryFn: () => ok<RemotePolicyRow[]>(api.GET("/api/admin/remote/policies")) });
  const [pattern, setPattern] = useState("");
  const [rule, setRule] = useState("consent_required");
  const [note, setNote] = useState("");
  const [imported, setImported] = useState<PolicyImportResult | null>(null);
  const [fileError, setFileError] = useState<unknown>(null);
  const refresh = () => void qc.invalidateQueries({ queryKey: ["remote-policies"] });
  const add = useMutation({
    mutationFn: () => ok(api.POST("/api/admin/remote/policies", { body: { device_pattern: pattern.trim(), rule, note: note.trim() } })),
    onSuccess: () => {
      setPattern("");
      setNote("");
      refresh();
    },
  });
  const del = useMutation({
    mutationFn: (id: string) => ok(api.DELETE("/api/admin/remote/policies/{id}", { params: { path: { id } } })),
    onSettled: refresh,
  });
  const imp = useMutation({
    mutationFn: (body: { ConsentRequiredComputers?: string[]; DeniedComputers?: string[] }) => ok<PolicyImportResult>(api.POST("/api/admin/remote/policies/import", { body })),
    onSuccess: (r) => {
      setImported(r);
      refresh();
    },
  });
  const onFile = async (f: File | undefined) => {
    setFileError(null);
    setImported(null);
    if (!f) return;
    try {
      const j = JSON.parse(await f.text()) as Record<string, unknown>;
      const list = (k: string) => (Array.isArray(j[k]) ? (j[k] as unknown[]).filter((x): x is string => typeof x === "string") : []);
      imp.mutate({ ConsentRequiredComputers: list("ConsentRequiredComputers"), DeniedComputers: list("DeniedComputers") });
    } catch {
      setFileError(new ApiError(400, "bad_json", t("remoteAdmin.policies.badFile")));
    }
  };
  return (
    <Section title={t("remoteAdmin.policies.title")} hint={t("remoteAdmin.policies.hint")}>
      <div className="flex flex-wrap items-end gap-2">
        <Field label={t("remoteAdmin.policies.pattern")}>
          <Input value={pattern} onChange={(e) => setPattern(e.target.value)} placeholder="BUH-* / kassa-pc" />
        </Field>
        <Field label={t("remoteAdmin.policies.rule")}>
          <Select value={rule} onChange={(e) => setRule(e.target.value)}>
            <option value="consent_required">{t("remote.rule.consent_required")}</option>
            <option value="deny">{t("remote.rule.deny")}</option>
          </Select>
        </Field>
        <Field label={t("remoteAdmin.policies.note")}>
          <Input value={note} onChange={(e) => setNote(e.target.value)} />
        </Field>
        <Button variant="primary" disabled={!pattern.trim() || add.isPending} onClick={() => add.mutate()}>
          {t("common.add")}
        </Button>
      </div>
      <label className="mt-3 inline-flex cursor-pointer items-center gap-2 text-sm text-accent">
        <input type="file" accept=".json,application/json" className="hidden" onChange={(e) => void onFile(e.target.files?.[0])} />
        {t("remoteAdmin.policies.import")}
      </label>
      {imported && (
        <p className="mt-2 text-sm">
          {t("remoteAdmin.policies.imported", { added: imported.added, skipped: imported.skipped })}
          {imported.invalid.length > 0 && <span className="text-bad"> {t("remoteAdmin.policies.invalid", { list: imported.invalid.join(", ") })}</span>}
        </p>
      )}
      {!!(add.error ?? del.error ?? imp.error ?? fileError) && <ErrorBox error={add.error ?? del.error ?? imp.error ?? fileError} />}
      {q.data && q.data.length > 0 && (
        <ul className="mt-3 divide-y divide-line text-sm">
          {q.data.map((p) => (
            <li key={p.id} className="flex flex-wrap items-center gap-2 py-2">
              <span className="font-mono">{p.device_pattern}</span>
              <Badge tone={p.rule === "deny" ? "bad" : "accent"}>{dyn(t, `remote.rule.${p.rule}`)}</Badge>
              <span className="text-xs text-muted">{p.note}</span>
              <Button variant="ghost" className="ml-auto" disabled={del.isPending} onClick={() => window.confirm(t("common.confirmDelete")) && del.mutate(p.id)}>
                {t("common.delete")}
              </Button>
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

const KINDS = ["agent_msi", "helper", "agent_update"] as const;

function PackagesSection() {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["remote-packages"], queryFn: () => ok<AgentPackageRow[]>(api.GET("/api/admin/remote/packages")) });
  const [kind, setKind] = useState<(typeof KINDS)[number]>("agent_msi");
  const [version, setVersion] = useState("");
  const [signature, setSignature] = useState("");
  const [file, setFile] = useState<File | null>(null);
  const refresh = () => void qc.invalidateQueries({ queryKey: ["remote-packages"] });
  const upload = useMutation({
    // Тело — сам файл (до 64 МБ), параметры — в строке запроса.
    mutationFn: async () => {
      if (!file) throw new Error("no file");
      const qs = new URLSearchParams({ kind, version: version.trim(), file_name: file.name });
      if (kind === "agent_update") qs.set("signature", signature.trim());
      const headers: Record<string, string> = { "Content-Type": "application/octet-stream" };
      const csrf = csrfToken();
      if (csrf) headers["X-CSRF-Token"] = csrf;
      const r = await fetch(`/api/admin/remote/packages?${qs}`, { method: "POST", credentials: "same-origin", headers, body: file });
      if (!r.ok) {
        const b = (await r.json().catch(() => ({}))) as { error?: string; message?: string };
        throw new ApiError(r.status, b.error ?? `http_${r.status}`, b.message ?? r.statusText);
      }
    },
    onSuccess: () => {
      setFile(null);
      setVersion("");
      setSignature("");
      refresh();
    },
  });
  const current = useMutation({
    mutationFn: (id: string) => ok(api.POST("/api/admin/remote/packages/{id}/current", { params: { path: { id } } })),
    onSettled: () => {
      refresh();
      void qc.invalidateQueries({ queryKey: ["remote-settings"] });
    },
  });
  return (
    <Section title={t("remoteAdmin.packages.title")} hint={t("remoteAdmin.packages.hint")}>
      <div className="flex flex-wrap items-end gap-2">
        <Field label={t("remoteAdmin.packages.kind")}>
          <Select value={kind} onChange={(e) => setKind(e.target.value as (typeof KINDS)[number])}>
            {KINDS.map((k) => (
              <option key={k} value={k}>
                {dyn(t, `remoteAdmin.packages.kinds.${k}`)}
              </option>
            ))}
          </Select>
        </Field>
        <Field label={t("remoteAdmin.packages.version")}>
          <Input className="w-28" value={version} onChange={(e) => setVersion(e.target.value)} placeholder="0.5.0" />
        </Field>
        <Field label={t("remoteAdmin.packages.file")}>
          <input type="file" className="text-sm" onChange={(e) => setFile(e.target.files?.[0] ?? null)} />
        </Field>
      </div>
      {kind === "agent_update" && (
        <div className="mt-2">
          <Field label={t("remoteAdmin.packages.signature")} hint={t("remoteAdmin.packages.signatureHint")}>
            <Input className="font-mono" value={signature} onChange={(e) => setSignature(e.target.value)} />
          </Field>
        </div>
      )}
      <Button className="mt-2" variant="primary" disabled={!file || !version.trim() || (kind === "agent_update" && !signature.trim()) || upload.isPending} onClick={() => upload.mutate()}>
        {t("remoteAdmin.packages.upload")}
      </Button>
      {(upload.error ?? current.error) && <ErrorBox error={upload.error ?? current.error} />}
      {q.data && q.data.length > 0 && (
        <ul className="mt-3 divide-y divide-line text-sm">
          {q.data.map((p) => (
            <li key={p.id} className="flex flex-wrap items-center gap-2 py-2">
              <span className="font-medium">{dyn(t, `remoteAdmin.packages.kinds.${p.kind}`)}</span>
              <span className="font-mono">{p.version}</span>
              <span className="text-xs text-muted">
                {p.file_name} · {Math.round(p.size / 1024)} KB · {formatDate(p.uploaded_at, i18n.language)} · {p.uploaded_by}
              </span>
              <span className="font-mono text-xs text-muted" title={p.sha256}>
                {p.sha256.slice(0, 12)}…
              </span>
              {p.signed && <Badge tone="ok">{t("remoteAdmin.packages.signed")}</Badge>}
              {p.is_current ? (
                <Badge tone="accent">{t("remoteAdmin.packages.current")}</Badge>
              ) : (
                <Button variant="ghost" className="ml-auto" disabled={current.isPending} onClick={() => (p.kind !== "agent_update" || window.confirm(t("remoteAdmin.packages.rolloutConfirm", { version: p.version }))) && current.mutate(p.id)}>
                  {p.kind === "agent_update" ? t("remoteAdmin.packages.rollout") : t("remoteAdmin.packages.makeCurrent")}
                </Button>
              )}
            </li>
          ))}
        </ul>
      )}
      <div className="mt-3 flex flex-wrap gap-3 text-sm">
        <a className="text-accent underline" href="/api/remote/packages/agent_msi/download">
          {t("remoteAdmin.packages.downloadMsi")}
        </a>
        <a className="text-accent underline" href="/api/remote/packages/helper/download">
          {t("remoteAdmin.packages.downloadHelper")}
        </a>
      </div>
    </Section>
  );
}

function DevicesSection() {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["remote-devices", "admin"], queryFn: () => ok<DeviceRow[]>(api.GET("/api/remote/devices", { params: { query: { limit: 500 } } })) });
  const off = useMutation({
    mutationFn: (id: string) => ok(api.POST("/api/admin/remote/devices/{id}/deactivate", { params: { path: { id } } })),
    onSettled: () => void qc.invalidateQueries({ queryKey: ["remote-devices"] }),
  });
  const on = useMutation({
    mutationFn: (id: string) => ok(api.POST("/api/admin/remote/devices/{id}/activate", { params: { path: { id } } })),
    onSettled: () => void qc.invalidateQueries({ queryKey: ["remote-devices"] }),
  });
  return (
    <Section title={t("remoteAdmin.devices.title")} hint={t("remoteAdmin.devices.hint")}>
      {q.isPending ? (
        <Spinner />
      ) : q.data?.length === 0 ? (
        <Empty>{t("remote.empty")}</Empty>
      ) : (
        <ul className="divide-y divide-line text-sm">
          {q.data?.map((d) => (
            <li key={d.id} className={`flex flex-wrap items-center gap-2 py-2 ${d.is_active ? "" : "opacity-50"}`}>
              <span className="font-mono font-medium">{d.hostname}</span>
              <Online online={d.online} />
              <span className="text-xs text-muted">
                {d.agent_version ?? "—"} · {formatDate(d.last_seen_at, i18n.language)}
              </span>
              {d.is_active ? (
                <Button variant="danger" className="ml-auto" disabled={off.isPending} onClick={() => window.confirm(t("remoteAdmin.devices.deactivateConfirm", { name: d.hostname })) && off.mutate(d.id)}>
                  {t("remoteAdmin.devices.deactivate")}
                </Button>
              ) : (
                <>
                  <Badge>{t("remote.inactive")}</Badge>
                  <Button className="ml-auto" disabled={on.isPending} onClick={() => on.mutate(d.id)}>
                    {t("remoteAdmin.devices.activate")}
                  </Button>
                </>
              )}
            </li>
          ))}
        </ul>
      )}
      {(q.error ?? off.error ?? on.error) && <ErrorBox error={q.error ?? off.error ?? on.error} />}
    </Section>
  );
}

export function RemoteAdminPage() {
  const { t } = useTranslation();
  return (
    <div className="mx-auto max-w-4xl space-y-4">
      <PageHeader title={t("admin.menu.remote")} />
      <TokensSection />
      <PoliciesSection />
      <PackagesSection />
      <DevicesSection />
      <SettingsSection />
    </div>
  );
}
