// Админка: запись в AD (учётка записи, разрешённые OU и группы) и ресурсы для запросов доступа.
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type DirectoryConfig, type ResourceInput, type ResourceView } from "../../api/client";
import { Badge, Button, Card, Dialog, Empty, ErrorBox, Field, Input, PageHeader, Select, Spinner } from "../../ui";
import { dyn } from "../helpdesk/helpdeskUtil";
import { PersonPicker } from "../helpdesk/NewTicketPage";
import { fullName } from "../phonebookUtil";

const lines = (s: string) =>
  s
    .split("\n")
    .map((l) => l.trim())
    .filter(Boolean);

export function DirectoryAdminPage() {
  const q = useQuery({ queryKey: ["directory-config"], queryFn: () => ok<DirectoryConfig>(api.GET("/api/admin/directory")) });
  if (q.error) return <ErrorBox error={q.error} onRetry={() => void q.refetch()} />;
  if (q.isPending) return <Spinner />;
  return <DirectoryForm data={q.data} />;
}

function DirectoryForm({ data }: { data: DirectoryConfig }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [writer, setWriter] = useState(data.writer_bind_dn ?? "");
  const [password, setPassword] = useState("");
  const [ous, setOus] = useState(data.user_ous.join("\n"));
  const [groups, setGroups] = useState(data.groups.join("\n"));
  const [requireTicket, setRequireTicket] = useState(data.require_ticket);
  const save = useMutation({
    meta: { toast: t("common.saved") },
    mutationFn: () =>
      ok<DirectoryConfig>(
        api.PUT("/api/admin/directory", {
          body: { writer_bind_dn: writer.trim() || null, writer_password: password || null, user_ous: lines(ous), groups: lines(groups), require_ticket: requireTicket },
        }),
      ),
    onSuccess: () => {
      setPassword("");
      void qc.invalidateQueries({ queryKey: ["directory-config"] });
    },
  });
  return (
    <div className="mx-auto max-w-3xl space-y-4">
      <PageHeader title={t("admin.directory.title")}>
        {data.ldaps ? <Badge tone="ok">LDAPS</Badge> : <Badge tone="bad">{t("admin.directory.noLdaps")}</Badge>}
      </PageHeader>
      <p className="text-sm text-muted">{t("admin.directory.intro")}</p>
      <Card className="grid gap-4 p-5">
        <Field label={t("admin.directory.writer")} hint={t("admin.directory.writerHint")}>
          <Input value={writer} onChange={(e) => setWriter(e.target.value)} placeholder="CN=svc-pult-write,OU=Service,DC=…" />
        </Field>
        <Field label={t("admin.directory.writerPassword")} hint={data.has_writer_password ? t("admin.ad.passwordKept") : undefined}>
          <Input type="password" autoComplete="new-password" value={password} onChange={(e) => setPassword(e.target.value)} />
        </Field>
        <Field label={t("admin.directory.ous")} hint={t("admin.directory.ousHint")}>
          <textarea
            rows={4}
            value={ous}
            onChange={(e) => setOus(e.target.value)}
            className="w-full rounded-lg border border-control bg-paper px-3 py-2 font-mono text-xs focus:border-accent"
            placeholder="OU=Users,DC=…"
          />
        </Field>
        <Field label={t("admin.directory.groups")} hint={t("admin.directory.groupsHint")}>
          <textarea
            rows={4}
            value={groups}
            onChange={(e) => setGroups(e.target.value)}
            className="w-full rounded-lg border border-control bg-paper px-3 py-2 font-mono text-xs focus:border-accent"
            placeholder="CN=…,OU=Groups,DC=…"
          />
        </Field>
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={requireTicket} onChange={(e) => setRequireTicket(e.target.checked)} />
          {t("admin.directory.requireTicket")}
        </label>
        <div>
          {save.error && <ErrorBox error={save.error} />}
          <Button variant="primary" onClick={() => save.mutate()} disabled={save.isPending}>
            {t("common.save")}
          </Button>
        </div>
      </Card>
    </div>
  );
}

const KINDS = ["ad_group", "share", "application", "other"] as const;

type Editing = { id: string | null; input: ResourceInput; ownerName: string };

export function AccessResourcesPage() {
  const { t } = useTranslation();
  const q = useQuery({ queryKey: ["access-resources-admin"], queryFn: () => ok<ResourceView[]>(api.GET("/api/admin/helpdesk/access/resources")) });
  const [edit, setEdit] = useState<Editing | null>(null);
  const blank: Editing = { id: null, ownerName: "", input: { kind: "ad_group", name: "", description: "", group_dn: "", owner_id: "", is_active: true } };
  return (
    <div className="mx-auto max-w-4xl">
      <PageHeader title={t("admin.menu.accessResources")}>
        <Button variant="primary" onClick={() => setEdit(blank)}>
          {t("common.add")}
        </Button>
      </PageHeader>
      <p className="mb-4 text-sm text-muted">{t("admin.access.intro")}</p>
      {q.error && <ErrorBox error={q.error} onRetry={() => void q.refetch()} />}
      {q.isPending && <Spinner />}
      {q.data && q.data.length === 0 && <Empty>{t("admin.access.empty")}</Empty>}
      {q.data && q.data.length > 0 && (
        <Card className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead className="border-b border-line text-left text-xs text-muted">
              <tr>
                <th className="px-3 py-2 font-medium">{t("access.resource")}</th>
                <th className="px-3 py-2 font-medium">{t("admin.access.owner")}</th>
                <th className="px-3 py-2 font-medium" />
              </tr>
            </thead>
            <tbody>
              {q.data.map((r) => (
                <tr key={r.id} className={`border-b border-line last:border-0 ${r.is_active ? "" : "text-muted"}`}>
                  <td className="px-3 py-2">
                    <div className="font-medium">{r.name}</div>
                    <div className="text-xs text-muted">
                      {dyn(t, `access.kind.${r.kind}`)}
                      {r.group_dn && <span className="ml-2 font-mono">{r.group_dn}</span>}
                    </div>
                  </td>
                  <td className="px-3 py-2">{r.owner_name ?? "—"}</td>
                  <td className="px-3 py-2 text-right">
                    {!r.is_active && <Badge>{t("admin.access.inactive")}</Badge>}{" "}
                    <Button
                      variant="ghost"
                      onClick={() =>
                        setEdit({
                          id: r.id,
                          ownerName: r.owner_name ?? "",
                          input: { kind: r.kind, name: r.name, description: r.description, group_dn: r.group_dn ?? "", owner_id: r.owner_id, is_active: r.is_active },
                        })
                      }
                    >
                      {t("common.edit")}
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </Card>
      )}
      {edit && <ResourceDialog edit={edit} onClose={() => setEdit(null)} />}
    </div>
  );
}

function ResourceDialog({ edit, onClose }: { edit: Editing; onClose: () => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [v, setV] = useState(edit.input);
  const [ownerName, setOwnerName] = useState(edit.ownerName);
  const save = useMutation({
    meta: { toast: t("common.saved") },
    mutationFn: () => {
      const body = { ...v, group_dn: v.group_dn?.trim() || null };
      return edit.id
        ? ok(api.PUT("/api/admin/helpdesk/access/resources/{id}", { params: { path: { id: edit.id } }, body }))
        : ok(api.POST("/api/admin/helpdesk/access/resources", { body }));
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["access-resources-admin"] });
      void qc.invalidateQueries({ queryKey: ["access-resources"] });
      onClose();
    },
  });
  return (
    <Dialog open onClose={onClose} title={edit.id ? t("common.edit") : t("common.add")}>
      <div className="space-y-3">
        <Field label={t("admin.access.kind")}>
          <Select value={v.kind} onChange={(e) => setV({ ...v, kind: e.target.value })}>
            {KINDS.map((k) => (
              <option key={k} value={k}>
                {dyn(t, `access.kind.${k}`)}
              </option>
            ))}
          </Select>
        </Field>
        <Field label={t("admin.access.name")}>
          <Input value={v.name} maxLength={200} onChange={(e) => setV({ ...v, name: e.target.value })} />
        </Field>
        <Field label={t("admin.access.description")}>
          <Input value={v.description} maxLength={2000} onChange={(e) => setV({ ...v, description: e.target.value })} />
        </Field>
        <Field label={t("directory.groupDn")} hint={v.kind === "ad_group" ? t("admin.access.groupHint") : t("admin.access.groupOptional")}>
          <Input value={v.group_dn ?? ""} onChange={(e) => setV({ ...v, group_dn: e.target.value })} placeholder="CN=…,OU=Groups,DC=…" />
        </Field>
        <Field label={t("admin.access.owner")} hint={t("admin.access.ownerHint")}>
          {v.owner_id ? (
            <div className="flex items-center gap-2 rounded-lg border border-line px-3 py-2 text-sm">
              <span className="font-medium">{ownerName || "—"}</span>
              <Button variant="ghost" className="ml-auto" onClick={() => setV({ ...v, owner_id: "" })}>
                {t("common.change")}
              </Button>
            </div>
          ) : (
            <PersonPicker
              value={null}
              onPick={(p) => {
                if (!p) return;
                setV({ ...v, owner_id: p.id });
                setOwnerName(fullName(p));
              }}
            />
          )}
        </Field>
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={v.is_active} onChange={(e) => setV({ ...v, is_active: e.target.checked })} />
          {t("admin.access.active")}
        </label>
        {save.error && <ErrorBox error={save.error} />}
        <div className="flex justify-end gap-2">
          <Button onClick={onClose}>{t("common.cancel")}</Button>
          <Button variant="primary" onClick={() => save.mutate()} disabled={save.isPending || !v.name.trim() || !v.owner_id}>
            {t("common.save")}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}
