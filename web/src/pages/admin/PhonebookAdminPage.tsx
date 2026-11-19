// Админка → Справочник: подразделения и должности, источники полей; связывание с AD.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type DictItem, type FieldSource, type LinkCandidate, type LinkSettings } from "../../api/client";
import { can, useMe } from "../../session";
import { Badge, Button, Card, Empty, ErrorBox, Input, PageHeader, Spinner } from "../../ui";

type DictKind = "departments" | "positions";

function DictEditor({ kind }: { kind: DictKind }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const path = kind === "departments" ? "/api/phonebook/departments" : "/api/phonebook/positions";
  const q = useQuery({ queryKey: [kind, "all"], queryFn: () => ok<DictItem[]>(api.GET(path, { params: { query: { archived: true } } })) });
  const [name, setName] = useState("");
  const [rename, setRename] = useState<{ id: string; name: string } | null>(null);
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: [kind] });
    void qc.invalidateQueries({ queryKey: ["persons"] });
  };
  const create = useMutation({
    mutationFn: () => ok(api.POST(path, { body: { name, parent_id: null } })),
    onSuccess: () => {
      setName("");
      refresh();
    },
  });
  const update = useMutation({
    mutationFn: (d: DictItem & { newName: string }) =>
      ok(
        kind === "departments"
          ? api.PUT("/api/phonebook/departments/{id}", { params: { path: { id: d.id } }, body: { version: d.version, name: d.newName, parent_id: d.parent_id ?? null } })
          : api.PUT("/api/phonebook/positions/{id}", { params: { path: { id: d.id } }, body: { version: d.version, name: d.newName, parent_id: null } }),
      ),
    onSuccess: () => {
      setRename(null);
      refresh();
    },
  });
  const archive = useMutation({
    mutationFn: (d: DictItem) =>
      ok(
        kind === "departments"
          ? api.POST("/api/phonebook/departments/{id}/archive", { params: { path: { id: d.id } }, body: { version: d.version, archived: !d.is_archived } })
          : api.POST("/api/phonebook/positions/{id}/archive", { params: { path: { id: d.id } }, body: { version: d.version, archived: !d.is_archived } }),
      ),
    onSuccess: refresh,
  });
  const remove = useMutation({
    mutationFn: (d: DictItem) =>
      ok(
        kind === "departments"
          ? api.DELETE("/api/phonebook/departments/{id}", { params: { path: { id: d.id } } })
          : api.DELETE("/api/phonebook/positions/{id}", { params: { path: { id: d.id } } }),
      ),
    onSuccess: refresh,
  });
  const error = create.error ?? update.error ?? archive.error ?? remove.error;
  return (
    <Card className="p-4">
      <h2 className="mb-3 font-semibold">{t(`admin.phonebook.${kind}`)}</h2>
      <form
        className="mb-3 flex gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          if (name.trim()) create.mutate();
        }}
      >
        <Input value={name} onChange={(e) => setName(e.target.value)} placeholder={t("admin.phonebook.newName")} />
        <Button type="submit" disabled={!name.trim() || create.isPending}>
          {t("common.create")}
        </Button>
      </form>
      {error && <ErrorBox error={error} />}
      {q.isPending ? (
        <Spinner />
      ) : (
        <ul className="max-h-96 divide-y divide-line overflow-y-auto text-sm">
          {q.data?.map((d) => (
            <li key={d.id} className="flex items-center gap-2 py-1.5">
              {rename?.id === d.id ? (
                <form
                  className="flex flex-1 gap-2"
                  onSubmit={(e) => {
                    e.preventDefault();
                    update.mutate({ ...d, newName: rename.name });
                  }}
                >
                  <Input autoFocus value={rename.name} onChange={(e) => setRename({ id: d.id, name: e.target.value })} />
                  <Button type="submit" variant="primary">
                    {t("common.save")}
                  </Button>
                </form>
              ) : (
                <>
                  <span className={`flex-1 ${d.is_archived ? "text-muted line-through" : ""}`}>{d.name}</span>
                  <Badge>{d.persons}</Badge>
                  <Button variant="ghost" onClick={() => setRename({ id: d.id, name: d.name })}>
                    {t("common.edit")}
                  </Button>
                  <Button variant="ghost" onClick={() => archive.mutate(d)}>
                    {d.is_archived ? t("common.restore") : t("common.archive")}
                  </Button>
                  {d.persons === 0 && (
                    <Button variant="ghost" disabled={remove.isPending} onClick={() => window.confirm(t("common.confirmDelete")) && remove.mutate(d)}>
                      {t("common.delete")}
                    </Button>
                  )}
                </>
              )}
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

const FIELDS = ["last_name", "first_name", "middle_name", "email", "department", "position", "work_phone", "mobile_phone", "site", "room"] as const;

function FieldSources() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["field-sources"], queryFn: () => ok<Record<string, FieldSource>>(api.GET("/api/admin/phonebook/field-sources")) });
  const save = useMutation({
    mutationFn: (v: Record<string, FieldSource>) => ok(api.PUT("/api/admin/phonebook/field-sources", { body: v })),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["field-sources"] });
      void qc.invalidateQueries({ queryKey: ["phonebook-config"] });
    },
  });
  if (q.isPending) return <Spinner />;
  if (q.error) return <ErrorBox error={q.error} />;
  return (
    <Card className="p-4">
      <h2 className="mb-1 font-semibold">{t("admin.phonebook.sources")}</h2>
      <p className="mb-3 text-sm text-muted">{t("admin.phonebook.sourcesHint")}</p>
      <table className="w-full text-sm">
        <tbody>
          {FIELDS.map((f) => (
            <tr key={f} className="border-t border-line">
              <td className="py-1.5">{t(`person.field.${f}`)}</td>
              {(["ad", "portal"] as const).map((s) => (
                <td key={s} className="py-1.5">
                  <label className="flex items-center gap-1.5">
                    <input type="radio" name={f} checked={q.data[f] === s} onChange={() => save.mutate({ ...q.data, [f]: s })} />
                    {t(`admin.phonebook.source.${s}`)}
                  </label>
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      {save.error && <ErrorBox error={save.error} />}
    </Card>
  );
}

export function PhonebookAdminPage() {
  const { t } = useTranslation();
  const me = useMe().data;
  return (
    <div className="mx-auto max-w-5xl space-y-4">
      <PageHeader title={t("admin.phonebook.title")} />
      <div className="grid gap-4 md:grid-cols-2">
        <DictEditor kind="departments" />
        <DictEditor kind="positions" />
      </div>
      {can(me, "phonebook.admin") && <FieldSources />}
    </div>
  );
}

export function LinkingPage() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const q = useQuery({ queryKey: ["link-candidates"], queryFn: () => ok<LinkCandidate[]>(api.GET("/api/admin/people/link-candidates")) });
  const settings = useQuery({ queryKey: ["link-settings"], queryFn: () => ok<LinkSettings>(api.GET("/api/admin/people/link-settings")) });
  const decide = useMutation({
    mutationFn: ({ id, confirm }: { id: string; confirm: boolean }) =>
      ok(api.POST("/api/admin/people/link-candidates/{id}", { params: { path: { id } }, body: { confirm } })),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["link-candidates"] }),
  });
  const saveSettings = useMutation({
    mutationFn: (v: LinkSettings) => ok(api.PUT("/api/admin/people/link-settings", { body: v })),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["link-settings"] }),
  });
  return (
    <div className="mx-auto max-w-5xl space-y-4">
      <PageHeader title={t("admin.linking.title")} />
      {settings.data && (
        <Card className="p-4">
          <label className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              checked={settings.data.auto_create_persons}
              onChange={(e) => saveSettings.mutate({ auto_create_persons: e.target.checked })}
            />
            {t("admin.linking.autoCreate")}
          </label>
        </Card>
      )}
      {(q.error ?? decide.error) && <ErrorBox error={q.error ?? decide.error} />}
      {q.isPending ? (
        <Spinner />
      ) : q.data?.length === 0 ? (
        <Empty>{t("admin.linking.empty")}</Empty>
      ) : (
        <Card className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead className="border-b border-line text-left text-xs text-muted">
              <tr>
                <th className="px-3 py-2 font-medium">{t("admin.linking.account")}</th>
                <th className="px-3 py-2 font-medium">{t("admin.linking.person")}</th>
                <th className="px-3 py-2 font-medium">{t("admin.linking.reason")}</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {q.data?.map((c) => (
                <tr key={c.id} className="border-b border-line last:border-0">
                  <td className="px-3 py-2">
                    <div className="font-mono text-xs">{c.ad_login}</div>
                    <div>{c.ad_name}</div>
                    <div className="text-xs text-muted">{c.ad_mail}</div>
                  </td>
                  <td className="px-3 py-2">
                    <div>{c.person_name}</div>
                    <div className="text-xs text-muted">{[c.person_department, c.person_email].filter(Boolean).join(" · ")}</div>
                  </td>
                  <td className="px-3 py-2 text-muted">{c.reason}</td>
                  <td className="px-3 py-2 text-right whitespace-nowrap">
                    <Button variant="primary" onClick={() => decide.mutate({ id: c.id, confirm: true })}>
                      {t("admin.linking.confirm")}
                    </Button>{" "}
                    <Button variant="ghost" onClick={() => decide.mutate({ id: c.id, confirm: false })}>
                      {t("admin.linking.reject")}
                    </Button>
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
