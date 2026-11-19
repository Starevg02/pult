// Мастер настройки организации: после первого входа суперадминистратора новой установки.
// Шаги: язык и организация → модули → площадки → категории заявок → почта → готово.
// Каждый шаг сохраняется сразу; всё меняется потом в «Настройки → Система», мастер можно пройти заново.
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useId, useState, type ChangeEvent, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { api, ok, type InstallLanguageResult, type Modules, type OrgSettings } from "../api/client";
import { Logo } from "../auth";
import { asLang, LANGUAGES, setLanguage, type Lang } from "../i18n";
import { Button, Card, ErrorBox, Field, Input, Select } from "../ui";
import { Categories } from "./admin/HelpdeskAdminPage";
import { MailPage } from "./admin/MailPage";
import { SitesPage } from "./admin/SitesPage";

const LOGO_MAX = 256 * 1024;
const MODULES: readonly (keyof Modules)[] = ["helpdesk", "phonebook", "directory", "remote", "telephony"];

/** Сохранить организацию (название, модули, признак «мастер пройден»). */
function useSaveOrg(onSaved?: () => void) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (o: Pick<OrgSettings, "name" | "modules" | "setup_done">) => ok<OrgSettings>(api.PUT("/api/admin/org", { body: o })),
    onSuccess: (o) => {
      qc.setQueryData(["org"], o);
      onSaved?.();
    },
  });
}

/** Файл → base64 без префикса `data:…;base64,`. */
function toBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result).replace(/^data:[^,]*,/, ""));
    r.onerror = () => reject(r.error ?? new Error("read"));
    r.readAsDataURL(file);
  });
}

function LogoField({ org }: { org: OrgSettings }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [tooBig, setTooBig] = useState(false);
  const id = useId();
  const upload = useMutation({
    mutationFn: async (file: File) => ok<OrgSettings>(api.PUT("/api/admin/org/logo", { body: { data: await toBase64(file) } })),
    onSuccess: (o) => qc.setQueryData(["org"], o),
  });
  const remove = useMutation({
    mutationFn: () => ok<OrgSettings>(api.DELETE("/api/admin/org/logo")),
    onSuccess: (o) => qc.setQueryData(["org"], o),
  });
  const pick = (e: ChangeEvent<HTMLInputElement>) => {
    const f = e.target.files?.[0];
    e.target.value = "";
    if (!f) return;
    setTooBig(f.size > LOGO_MAX);
    if (f.size <= LOGO_MAX) upload.mutate(f);
  };
  // Своя кнопка вместо системной: подпись системного поля файла — на языке браузера, а не портала.
  return (
    <div>
      <span id={`${id}-label`} className="mb-1 block text-sm font-medium text-ink">
        {t("org.logo")}
      </span>
      <div className="flex flex-wrap items-center gap-3">
        {org.logo && <img src={`/api/org/logo?v=${org.logo}`} alt="" className="h-12 max-w-40 rounded border border-line object-contain" />}
        <label className="inline-flex cursor-pointer items-center rounded-lg border border-line-strong px-3 py-1.5 text-sm hover:bg-hover focus-within:ring-2 focus-within:ring-accent">
          {upload.isPending ? t("org.uploading") : t("org.chooseLogo")}
          <input
            type="file"
            accept="image/png,image/jpeg,image/webp"
            onChange={pick}
            disabled={upload.isPending}
            aria-labelledby={`${id}-label`}
            aria-describedby={`${id}-hint`}
            className="sr-only"
          />
        </label>
        {org.logo && (
          <Button variant="ghost" disabled={remove.isPending} onClick={() => remove.mutate()}>
            {t("org.removeLogo")}
          </Button>
        )}
      </div>
      <span id={`${id}-hint`} className="mt-1 block text-xs text-muted">
        {t("org.logoHint")}
      </span>
      {tooBig && <p className="mt-1 text-sm text-bad">{t("org.logoTooBig")}</p>}
      {upload.error && <ErrorBox error={upload.error} />}
      {remove.error && <ErrorBox error={remove.error} />}
    </div>
  );
}

/** Название, логотип и (в мастере) язык установки. */
export function OrgIdentityCard({ org, withLanguage, submitLabel, onSaved }: { org: OrgSettings; withLanguage: boolean; submitLabel?: string; onSaved?: () => void }) {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const [name, setName] = useState(org.name);
  const [lang, setLang] = useState<Lang>(asLang(i18n.language) ?? "ru");
  const [renamed, setRenamed] = useState<number | null>(null);
  const save = useSaveOrg(onSaved);
  const language = useMutation({
    mutationFn: async (locale: Lang) => {
      const r = await ok<InstallLanguageResult>(api.PUT("/api/admin/org/language", { body: { locale } }));
      // Язык суперадминистратора — тот же: на нём ему приходят письма.
      await api.PUT("/api/me/locale", { body: { locale } }).catch(() => undefined);
      return r;
    },
    onSuccess: (r) => {
      setRenamed(r.renamed);
      void qc.invalidateQueries();
    },
  });
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (withLanguage) {
      setLanguage(lang);
      try {
        await language.mutateAsync(lang);
      } catch {
        return; // ошибка показана под формой
      }
    }
    save.mutate({ name, modules: org.modules, setup_done: org.setup_done });
  };
  return (
    <form onSubmit={(e) => void submit(e)}>
      <Card className="space-y-4 p-5">
        {withLanguage && (
          <Field label={t("org.language")} hint={t("org.languageHint")}>
            <Select
              value={lang}
              onChange={(e) => {
                const l = asLang(e.target.value) ?? "ru";
                setLang(l);
                setLanguage(l);
              }}
            >
              {LANGUAGES.map(([code, label]) => (
                <option key={code} value={code}>
                  {label}
                </option>
              ))}
            </Select>
          </Field>
        )}
        <Field label={t("org.name")} hint={t("org.nameHint")}>
          <Input maxLength={100} value={name} onChange={(e) => setName(e.target.value)} />
        </Field>
        <LogoField org={org} />
        {renamed !== null && renamed > 0 && <p className="text-sm text-muted">{t("org.renamed", { n: renamed })}</p>}
        {language.error && <ErrorBox error={language.error} />}
        {save.error && <ErrorBox error={save.error} />}
        <Button type="submit" variant="primary" disabled={save.isPending || language.isPending}>
          {submitLabel ?? t("common.save")}
        </Button>
      </Card>
    </form>
  );
}

/** Включённые модули. Телефония — только вместе со справочником. */
export function ModulesCard({ org, submitLabel, onSaved }: { org: OrgSettings; submitLabel?: string; onSaved?: () => void }) {
  const { t } = useTranslation();
  const [m, setM] = useState<Modules>(org.modules);
  const save = useSaveOrg(onSaved);
  const toggle = (k: keyof Modules, v: boolean) => {
    const next = { ...m, [k]: v };
    if (!next.phonebook) next.telephony = false;
    setM(next);
  };
  return (
    <Card className="space-y-3 p-5">
      <p className="text-sm text-muted">{t("org.modulesText")}</p>
      <ul className="space-y-3">
        {MODULES.map((k) => {
          const blocked = k === "telephony" && !m.phonebook;
          return (
            <li key={k}>
              <label className={`flex gap-3 ${blocked ? "opacity-60" : ""}`}>
                <input type="checkbox" className="mt-1" checked={m[k]} disabled={blocked} onChange={(e) => toggle(k, e.target.checked)} />
                <span>
                  <span className="block font-medium">{t(`org.module.${k}`)}</span>
                  <span className="block text-sm text-muted">{blocked ? t("org.telephonyNeedsPhonebook") : t(`org.moduleAbout.${k}`)}</span>
                </span>
              </label>
            </li>
          );
        })}
      </ul>
      {save.error && <ErrorBox error={save.error} />}
      <Button variant="primary" disabled={save.isPending} onClick={() => save.mutate({ name: org.name, modules: m, setup_done: org.setup_done })}>
        {submitLabel ?? t("common.save")}
      </Button>
    </Card>
  );
}

type Step = "identity" | "modules" | "sites" | "categories" | "mail" | "done";

export function OrgWizard({ org }: { org: OrgSettings }) {
  const { t } = useTranslation();
  const [step, setStep] = useState<Step>("identity");
  const steps: Step[] = ["identity", "modules", "sites", ...(org.modules.helpdesk ? (["categories"] as Step[]) : []), "mail", "done"];
  const at = Math.max(steps.indexOf(step), 0);
  const go = (d: number) => setStep(steps[Math.min(Math.max(at + d, 0), steps.length - 1)] ?? "done");
  const finish = useSaveOrg();
  const nav = (
    <div className="mt-4 flex justify-between">
      <Button variant="ghost" disabled={at === 0} onClick={() => go(-1)}>
        ← {t("org.back")}
      </Button>
      {step !== "identity" && step !== "modules" && step !== "done" && (
        <Button variant="primary" onClick={() => go(1)}>
          {t("setup.next")} →
        </Button>
      )}
    </div>
  );
  return (
    <div className="min-h-screen px-4 py-8">
      <div className="mx-auto max-w-4xl">
        <div className="mb-4">
          <Logo />
        </div>
        <h1 className="text-xl font-semibold">{t("org.wizardTitle")}</h1>
        <p className="mb-4 text-sm text-muted">{t("org.wizardText")}</p>
        <ol className="mb-6 flex flex-wrap gap-2 text-sm">
          {steps.map((s, i) => (
            <li key={s}>
              <button
                type="button"
                aria-current={s === step ? "step" : undefined}
                className={`rounded-full px-3 py-1 ${s === step ? "bg-ink text-paper" : i < at ? "bg-accentsoft text-accent-ink" : "bg-hover text-muted"}`}
                onClick={() => setStep(s)}
              >
                {i + 1}. {t(`org.steps.${s}`)}
              </button>
            </li>
          ))}
        </ol>
        {step === "identity" && <OrgIdentityCard org={org} withLanguage submitLabel={`${t("setup.next")} →`} onSaved={() => go(1)} />}
        {step === "modules" && <ModulesCard org={org} submitLabel={`${t("setup.next")} →`} onSaved={() => go(1)} />}
        {step === "sites" && (
          <>
            <p className="mb-3 text-sm text-muted">{t("org.sitesText")}</p>
            <SitesPage />
          </>
        )}
        {step === "categories" && (
          <>
            <p className="mb-3 text-sm text-muted">{t("org.categoriesText")}</p>
            <Categories />
          </>
        )}
        {step === "mail" && (
          <>
            <p className="mb-3 text-sm text-muted">{t("org.mailText")}</p>
            <MailPage />
          </>
        )}
        {step === "done" && (
          <Card className="space-y-4 p-5">
            <p>{t("org.doneText")}</p>
            {finish.error && <ErrorBox error={finish.error} />}
            <Button variant="primary" disabled={finish.isPending} onClick={() => finish.mutate({ name: org.name, modules: org.modules, setup_done: true })}>
              {t("org.finish")}
            </Button>
          </Card>
        )}
        {nav}
      </div>
    </div>
  );
}
