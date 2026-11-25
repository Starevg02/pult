// Расширения: внешние сервисы по манифесту. Суперадминистратор видит запрошенные права до установки;
// токен API и секрет вебхука показываются один раз — их вставляют в настройки расширения.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type CatalogEntry, type Extension, type ExtensionSettings, type InstalledExtension, type ManifestPreview, type ManifestSource } from "../../api/client";
import { formatDate } from "../../format";
import { Badge, Button, Card, CopyButton, Dialog, Empty, ErrorBox, Field, Input, PageHeader, Spinner } from "../../ui";

type Source = ManifestSource;

function Secrets({ shown, onClose }: { shown: InstalledExtension; onClose: () => void }) {
  const { t } = useTranslation();
  return (
    <Dialog open onClose={onClose} title={t("extensions.secretsTitle")}>
      <p className="text-sm text-muted">{t("extensions.secretsHint")}</p>
      <div className="mt-3 grid gap-3">
        {shown.token && (
          <Field label={t("extensions.token")}>
            <div className="flex gap-2">
              <Input readOnly value={shown.token} className="font-mono" />
              <CopyButton value={shown.token} label={t("extensions.token")} />
            </div>
          </Field>
        )}
        {shown.webhook_secret && (
          <Field label={t("extensions.webhookSecret")}>
            <div className="flex gap-2">
              <Input readOnly value={shown.webhook_secret} className="font-mono" />
              <CopyButton value={shown.webhook_secret} label={t("extensions.webhookSecret")} />
            </div>
          </Field>
        )}
        {!shown.token && !shown.webhook_secret && <p className="text-sm">{t("extensions.nothingToShow")}</p>}
      </div>
      <div className="mt-3 flex justify-end">
        <Button variant="primary" onClick={onClose}>
          {t("extensions.saved")}
        </Button>
      </div>
    </Dialog>
  );
}

function Preview({ source, onClose, onInstalled }: { source: Source; onClose: () => void; onInstalled: (s: InstalledExtension) => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["extension-preview", source], queryFn: () => ok<ManifestPreview>(api.POST("/api/admin/extensions/preview", { body: source })), retry: false });
  const install = useMutation({
    mutationFn: () => ok<InstalledExtension>(api.POST("/api/admin/extensions", { body: source })),
    onSuccess: (s) => {
      void qc.invalidateQueries({ queryKey: ["extensions"] });
      void qc.invalidateQueries({ queryKey: ["extension-links"] });
      void qc.invalidateQueries({ queryKey: ["extension-catalog"] });
      onInstalled(s);
    },
  });
  const m = q.data?.manifest;
  return (
    <Dialog open onClose={onClose} title={m ? `${m.name} ${m.version}` : t("extensions.install")}>
      {q.error ? (
        <ErrorBox error={q.error} />
      ) : !q.data || !m ? (
        <Spinner />
      ) : (
        <div className="grid gap-3 text-sm">
          <div className="text-xs text-muted">
            {m.id}
            {m.publisher && ` · ${m.publisher}`}
          </div>
          {m.description && <p>{m.description}</p>}
          <div>
            <h3 className="font-semibold">{t("extensions.willRead")}</h3>
            {q.data.permissions.length === 0 ? (
              <p className="text-muted">{t("extensions.noApi")}</p>
            ) : (
              <ul className="mt-1 list-disc pl-5">
                {q.data.permissions.map((p) => (
                  <li key={p.code}>
                    {p.description} <span className="font-mono text-xs text-muted">{p.code}</span>
                  </li>
                ))}
              </ul>
            )}
          </div>
          {m.webhook && (
            <div>
              <h3 className="font-semibold">{t("extensions.willReceive")}</h3>
              <p className="break-all font-mono text-xs">{m.webhook.url}</p>
              <p className="text-xs text-muted">{(m.webhook.events ?? []).length > 0 ? (m.webhook.events ?? []).join(", ") : t("extensions.allEvents")}</p>
            </div>
          )}
          {(m.links ?? []).length > 0 && (
            <div>
              <h3 className="font-semibold">{t("extensions.menuLinks")}</h3>
              <ul className="mt-1 list-disc pl-5">
                {(m.links ?? []).map((l) => (
                  <li key={l.url}>
                    {l.title} <span className="break-all text-xs text-muted">{l.url}</span>
                  </li>
                ))}
              </ul>
            </div>
          )}
          <p className="text-xs text-muted">{t("extensions.responsibility")}</p>
          {q.data.already_installed && <p className="text-bad">{t("extensions.alreadyInstalled")}</p>}
          {install.error && <ErrorBox error={install.error} />}
          <div className="flex justify-end gap-2">
            <Button onClick={onClose}>{t("common.cancel")}</Button>
            <Button variant="primary" disabled={q.data.already_installed || install.isPending} onClick={() => install.mutate()}>
              {t("extensions.install")}
            </Button>
          </div>
        </div>
      )}
    </Dialog>
  );
}

function CatalogCard({ onPick }: { onPick: (s: Source) => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const s = useQuery({ queryKey: ["extension-settings"], queryFn: () => ok<ExtensionSettings>(api.GET("/api/admin/extensions/settings")) });
  const [url, setUrl] = useState<string | null>(null);
  const value = url ?? s.data?.catalog_url ?? "";
  const save = useMutation({
    meta: { toast: t("common.saved") },
    mutationFn: () => ok(api.PUT("/api/admin/extensions/settings", { body: { catalog_url: value } })),
    onSuccess: () => {
      setUrl(null);
      void qc.invalidateQueries({ queryKey: ["extension-settings"] });
      void qc.invalidateQueries({ queryKey: ["extension-catalog"] });
    },
  });
  const cat = useQuery({
    queryKey: ["extension-catalog", s.data?.catalog_url],
    queryFn: () => ok<CatalogEntry[]>(api.GET("/api/admin/extensions/catalog")),
    enabled: !!s.data?.catalog_url,
    retry: false,
  });
  return (
    <Card className="grid gap-3 p-4">
      <h2 className="font-semibold">{t("extensions.catalog")}</h2>
      <Field label={t("extensions.catalogUrl")} hint={t("extensions.catalogHint")}>
        <div className="flex gap-2">
          <Input value={value} onChange={(e) => setUrl(e.target.value)} placeholder="https://extensions.example.local/catalog.json" />
          <Button disabled={save.isPending || value === (s.data?.catalog_url ?? "")} onClick={() => save.mutate()}>
            {t("common.save")}
          </Button>
        </div>
      </Field>
      {save.error && <ErrorBox error={save.error} />}
      {cat.error ? (
        <ErrorBox error={cat.error} />
      ) : cat.data && cat.data.length > 0 ? (
        <ul className="divide-y divide-line rounded-lg border border-line">
          {cat.data.map((e) => (
            <li key={e.manifest.id} className="flex items-center gap-2 px-3 py-2 text-sm">
              <div className="min-w-0 flex-1">
                <div className="font-medium">
                  {e.manifest.name} <span className="text-muted">{e.manifest.version}</span>
                </div>
                {e.manifest.description && <div className="truncate text-xs text-muted">{e.manifest.description}</div>}
              </div>
              {e.installed_version ? (
                <Badge tone="ok">{t("extensions.installedVersion", { v: e.installed_version })}</Badge>
              ) : (
                <Button onClick={() => onPick({ manifest: e.manifest as unknown as Source["manifest"] })}>{t("extensions.install")}</Button>
              )}
            </li>
          ))}
        </ul>
      ) : null}
    </Card>
  );
}

export function ExtensionsPage() {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const [source, setSource] = useState<Source | null>(null);
  const [shown, setShown] = useState<InstalledExtension | null>(null);
  const [url, setUrl] = useState("");
  const [fileError, setFileError] = useState(false);
  const q = useQuery({ queryKey: ["extensions"], queryFn: () => ok<Extension[]>(api.GET("/api/admin/extensions")) });
  const toggle = useMutation({
    mutationFn: ({ id, enabled }: { id: string; enabled: boolean }) => ok<InstalledExtension>(api.PUT("/api/admin/extensions/{id}", { params: { path: { id } }, body: { enabled } })),
    onSuccess: (s, v) => {
      void qc.invalidateQueries({ queryKey: ["extensions"] });
      void qc.invalidateQueries({ queryKey: ["extension-links"] });
      if (v.enabled) setShown(s);
    },
  });
  const remove = useMutation({
    mutationFn: (id: string) => ok(api.DELETE("/api/admin/extensions/{id}", { params: { path: { id } } })),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["extensions"] });
      void qc.invalidateQueries({ queryKey: ["extension-links"] });
    },
  });
  const readFile = (f: File | undefined) => {
    setFileError(false);
    if (!f) return;
    void f.text().then((text) => {
      try {
        setSource({ manifest: JSON.parse(text) as Source["manifest"] });
      } catch {
        setFileError(true);
      }
    });
  };
  return (
    <div className="mx-auto max-w-4xl space-y-4">
      <PageHeader title={t("admin.menu.extensions")} subtitle={t("extensions.hint")} />
      <Card className="grid gap-3 p-4">
        <h2 className="font-semibold">{t("extensions.install")}</h2>
        <Field label={t("extensions.fromFile")}>
          <input type="file" accept="application/json,.json" className="text-sm" onChange={(e) => readFile(e.target.files?.[0])} />
        </Field>
        {fileError && <p className="text-sm text-bad">{t("extensions.badFile")}</p>}
        <Field label={t("extensions.fromUrl")}>
          <div className="flex gap-2">
            <Input value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://ext.example.local/pult-extension.json" />
            <Button disabled={!url.trim()} onClick={() => setSource({ url: url.trim() })}>
              {t("extensions.check")}
            </Button>
          </div>
        </Field>
      </Card>
      <CatalogCard onPick={setSource} />
      <Card>
        {q.error ? (
          <ErrorBox error={q.error} onRetry={() => void q.refetch()} />
        ) : q.isPending ? (
          <Spinner />
        ) : q.data.length === 0 ? (
          <Empty>{t("extensions.empty")}</Empty>
        ) : (
          <ul className="divide-y divide-line">
            {q.data.map((e) => (
              <li key={e.id} className="flex flex-wrap items-center gap-2 px-4 py-3 text-sm">
                <div className="min-w-0 flex-1">
                  <div className="font-medium">
                    {e.manifest.name} <span className="text-muted">{e.manifest.version}</span>
                  </div>
                  <div className="text-xs text-muted">
                    {e.manifest.id} · {e.installed_by_name} · {formatDate(e.installed_at, i18n.language)}
                    {e.token_last_used_at && ` · ${t("extensions.lastUsed", { at: formatDate(e.token_last_used_at, i18n.language) })}`}
                  </div>
                  {e.webhook_last_error && <div className="text-xs text-bad">{e.webhook_last_error}</div>}
                </div>
                <Badge tone={e.enabled ? "ok" : "muted"}>{e.enabled ? t("extensions.on") : t("extensions.off")}</Badge>
                <Button variant="ghost" disabled={toggle.isPending} onClick={() => toggle.mutate({ id: e.id, enabled: !e.enabled })}>
                  {e.enabled ? t("extensions.disable") : t("extensions.enable")}
                </Button>
                <Button variant="danger" disabled={remove.isPending} onClick={() => window.confirm(t("extensions.confirmDelete", { name: e.manifest.name })) && remove.mutate(e.id)}>
                  {t("common.delete")}
                </Button>
              </li>
            ))}
          </ul>
        )}
        {(toggle.error ?? remove.error) && <ErrorBox error={toggle.error ?? remove.error} />}
      </Card>
      {source && (
        <Preview
          source={source}
          onClose={() => setSource(null)}
          onInstalled={(s) => {
            setSource(null);
            setShown(s);
          }}
        />
      )}
      {shown && <Secrets shown={shown} onClose={() => setShown(null)} />}
    </div>
  );
}
