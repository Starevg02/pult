// Админка → Площадки: адреса, часовые пояса, ответственные L1/L2.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ApiError, ok, type Engineer, type EngineerCandidate, type Site, type SiteInput } from "../../api/client";
import { Badge, Button, Card, Dialog, Empty, ErrorBox, Field, Input, PageHeader, Spinner } from "../../ui";

export function SitesPage() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const sites = useQuery({ queryKey: ["sites"], queryFn: () => ok<Site[]>(api.GET("/api/sites")) });
  const [editing, setEditing] = useState<Site | "new" | null>(null);
  return (
    <div className="mx-auto max-w-4xl">
      <PageHeader title={t("admin.sites.title")}>
        <Button variant="primary" onClick={() => setEditing("new")}>
          {t("admin.sites.new")}
        </Button>
      </PageHeader>
      {sites.error && <ErrorBox error={sites.error} />}
      {sites.isPending ? (
        <Spinner />
      ) : sites.data?.length === 0 ? (
        <Empty>{t("admin.sites.empty")}</Empty>
      ) : (
        <Card>
          <ul className="divide-y divide-line">
            {sites.data?.map((s) => (
              <li key={s.id} className="px-4 py-3 text-sm">
                <div className="flex flex-wrap items-center gap-3">
                <div className="min-w-0 flex-1">
                  <div className="font-medium">
                    {s.name} {!s.is_active && <Badge>{t("admin.sites.inactive")}</Badge>}
                  </div>
                  <div className="text-xs text-muted">{[s.address, s.timezone].filter(Boolean).join(" · ")}</div>
                </div>
                <Button variant="ghost" onClick={() => setEditing(s)}>
                  {t("common.edit")}
                </Button>
              </div>
                <SiteEngineers site={s} />
              </li>
            ))}
          </ul>
        </Card>
      )}
      {editing && (
        <SiteDialog
          site={editing === "new" ? null : editing}
          onClose={() => setEditing(null)}
          onSaved={() => {
            setEditing(null);
            void qc.invalidateQueries({ queryKey: ["sites"] });
          }}
        />
      )}
    </div>
  );
}

function SiteDialog({ site, onClose, onSaved }: { site: Site | null; onClose: () => void; onSaved: () => void }) {
  const { t } = useTranslation();
  const [v, setV] = useState<SiteInput>({
    name: site?.name ?? "",
    address: site?.address ?? null,
    timezone: site?.timezone ?? "Europe/Moscow",
    emergency_phone: site?.emergency_phone ?? null,
    is_active: site?.is_active ?? true,
    version: site?.version ?? 0,
  });
  const save = useMutation({
    mutationFn: () =>
      site ? ok(api.PUT("/api/admin/sites/{id}", { params: { path: { id: site.id } }, body: v })) : ok(api.POST("/api/admin/sites", { body: v })),
    onSuccess: onSaved,
  });
  const conflict = save.error instanceof ApiError && save.error.code === "conflict";
  return (
    <Dialog open onClose={onClose} title={site ? t("admin.sites.edit") : t("admin.sites.new")}>
      <div className="space-y-3">
        <Field label={t("admin.sites.name")}>
          <Input value={v.name} onChange={(e) => setV({ ...v, name: e.target.value })} />
        </Field>
        <Field label={t("admin.sites.address")}>
          <Input value={v.address ?? ""} onChange={(e) => setV({ ...v, address: e.target.value || null })} />
        </Field>
        <Field label={t("admin.sites.emergency")} hint={t("admin.sites.emergencyHint")}>
          <Input value={v.emergency_phone ?? ""} onChange={(e) => setV({ ...v, emergency_phone: e.target.value || null })} />
        </Field>
        <Field label={t("admin.sites.timezone")} hint="Europe/Moscow, Asia/Yekaterinburg…">
          <Input value={v.timezone} onChange={(e) => setV({ ...v, timezone: e.target.value })} />
        </Field>
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={v.is_active} onChange={(e) => setV({ ...v, is_active: e.target.checked })} />
          {t("admin.sites.active")}
        </label>
        {save.error && <ErrorBox error={conflict ? new ApiError(409, "conflict", t("common.conflictText")) : save.error} />}
      </div>
      <div className="mt-4 flex justify-end gap-2">
        <Button onClick={onClose}>{t("common.cancel")}</Button>
        <Button variant="primary" onClick={() => save.mutate()} disabled={save.isPending}>
          {t("common.save")}
        </Button>
      </div>
    </Dialog>
  );
}

/** Инженеры линии площадки: кого убрать — крестик, кого добавить — поиск по всем учёткам AD. */
function Line({ site, line, list, onChange }: { site: Site; line: "l1" | "l2"; list: Engineer[]; onChange: (e: Engineer[]) => void }) {
  const { t } = useTranslation();
  const [adding, setAdding] = useState(false);
  const [q, setQ] = useState("");
  const found = useQuery({
    queryKey: ["engineer-candidates", q],
    queryFn: () => ok<EngineerCandidate[]>(api.GET("/api/admin/engineers/candidates", { params: { query: { q } } })),
    enabled: adding && q.trim().length > 1,
  });
  const add = useMutation({
    mutationFn: (ad_account_id: string) => ok<Engineer[]>(api.POST("/api/admin/sites/{id}/engineers", { params: { path: { id: site.id } }, body: { ad_account_id, line } })),
    onSuccess: (e) => {
      onChange(e);
      setAdding(false);
      setQ("");
    },
  });
  const remove = useMutation({
    mutationFn: (user_id: string) => ok<Engineer[]>(api.DELETE("/api/admin/sites/{id}/engineers/{user_id}", { params: { path: { id: site.id, user_id }, query: { line } } })),
    onSuccess: onChange,
  });
  const here = list.filter((e) => e.line === line);
  return (
    <div className="flex flex-wrap items-center gap-2 text-sm">
      <span className="w-40 shrink-0 text-muted">{line === "l1" ? t("admin.sites.lineL1") : t("admin.sites.lineL2")}</span>
      {here.length === 0 && <span className="text-xs text-bad">{line === "l1" ? t("admin.sites.noL1") : t("admin.sites.noL2")}</span>}
      {here.map((e) => (
        <span key={e.user_id} className="inline-flex items-center gap-1 rounded-md bg-sunken px-2 py-0.5">
          {e.name}
          <button type="button" aria-label={t("common.delete")} className="text-muted hover:text-bad" onClick={() => remove.mutate(e.user_id)}>
            ×
          </button>
        </span>
      ))}
      <Button variant="ghost" onClick={() => setAdding(!adding)}>
        {t("admin.sites.addEngineer")}
      </Button>
      {adding && (
        <div className="w-full pl-0 md:pl-42">
          <Input autoFocus placeholder={t("admin.sites.searchEngineer")} value={q} onChange={(e) => setQ(e.target.value)} />
          {found.data && (
            <ul className="mt-1 divide-y divide-line rounded-lg border border-line">
              {found.data.length === 0 && <li className="px-3 py-2 text-muted">{t("phonebook.empty")}</li>}
              {found.data.map((c) => (
                <li key={c.ad_account_id}>
                  <button type="button" className="w-full px-3 py-2 text-left hover:bg-hover" onClick={() => add.mutate(c.ad_account_id)}>
                    {c.name} <span className="font-mono text-xs text-muted">{c.login}</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
      {(add.error ?? remove.error) && <div className="w-full"><ErrorBox error={add.error ?? remove.error} /></div>}
    </div>
  );
}

function SiteEngineers({ site }: { site: Site }) {
  const qc = useQueryClient();
  const key = ["site-engineers", site.id];
  const q = useQuery({ queryKey: key, queryFn: () => ok<Engineer[]>(api.GET("/api/admin/sites/{id}/engineers", { params: { path: { id: site.id } } })) });
  const set = (e: Engineer[]) => qc.setQueryData(key, e);
  return (
    <div className="mt-2 space-y-2 border-t border-line pt-2">
      <Line site={site} line="l1" list={q.data ?? []} onChange={set} />
      <Line site={site} line="l2" list={q.data ?? []} onChange={set} />
    </div>
  );
}
