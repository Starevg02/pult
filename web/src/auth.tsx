// Вход паролем AD, текущий пользователь, экран «Нет связи с доменом».
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState, type FormEvent, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { api, ApiError, ok, type Me, type SetupStatus } from "./api/client";
import { asLang, LANGUAGES, setLanguage } from "./i18n";
import { LoginHelpForm } from "./pages/helpdesk/PublicPages";
import { SetupWizard } from "./pages/SetupWizard";
import { useMe, useModule, useOrg } from "./session";
import { Button, Field, Input, Spinner } from "./ui";

const loginCodes = [
  "invalid_credentials",
  "account_disabled",
  "account_locked",
  "account_expired",
  "password_expired",
  "logon_hours",
  "workstation",
  "too_many_requests",
  "ad_unavailable",
  "ad_not_configured",
  "forbidden",
  "otp_required",
  "invalid_otp",
] as const;

/** Вход через домен: браузер сам отвечает на `WWW-Authenticate: Negotiate` билетом Kerberos. */
const kerberosCodes = ["negotiate_required", "ntlm_not_supported", "rejected", "kerberos_external", "forbidden", "account_disabled", "busy", "not_available"] as const;

/** Вход через провайдера OpenID Connect: сервер возвращает на страницу входа с `?oidc_error=<код>`. */
const oidcCodes = ["state", "denied", "external", "unavailable", "token", "no_account", "ambiguous", "account_disabled", "identity_mismatch", "otp_required", "error"] as const;

function takeOidcError() {
  const c = new URLSearchParams(window.location.search).get("oidc_error");
  if (!c) return null;
  return oidcCodes.find((k) => k === c) ?? "error";
}

export function LoginPage() {
  const { t } = useTranslation();
  const [oidcError] = useState(takeOidcError);
  // Код ошибки показан — из адреса его убираем, чтобы он не остался после входа и в закладках.
  useEffect(() => {
    if (oidcError) window.history.replaceState(null, "", window.location.pathname);
  }, [oidcError]);
  const qc = useQueryClient();
  const setup = useQuery({ queryKey: ["setup"], queryFn: () => ok<SetupStatus>(api.GET("/api/setup/status")), retry: false });
  const domain = useMutation({
    mutationFn: () => ok(api.GET("/api/auth/kerberos")),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["me"] }),
  });
  const domainCode = domain.error instanceof ApiError ? domain.error.code : null;
  const domainKnown = domainCode && (kerberosCodes as readonly string[]).includes(domainCode) ? (domainCode as (typeof kerberosCodes)[number]) : null;
  const [login, setLogin] = useState("");
  const [password, setPassword] = useState("");
  const [otp, setOtp] = useState("");
  const [help, setHelp] = useState(false);
  // Код из приложения сервер спрашивает только снаружи и только у тех, кто его подключил (или чья роль
  // обязывает): поле появляется после ответа otp_required и остаётся до входа.
  const [otpAsked, setOtpAsked] = useState(false);
  // «Не получается войти» создаёт заявку: без модуля заявок ссылки нет.
  const helpdesk = useModule("helpdesk");
  const m = useMutation({
    mutationFn: (code: string | null) => ok(api.POST("/api/auth/login", { body: { login, password, code } })),
    onSuccess: async () => {
      setPassword("");
      setOtp("");
      await qc.invalidateQueries({ queryKey: ["me"] });
    },
    onError: (e) => {
      const c = e instanceof ApiError ? e.code : null;
      if (c === "otp_required" || c === "invalid_otp") setOtpAsked(true);
      // Просьба ввести код — не ошибка пароля: введённый пароль оставляем.
      if (c !== "otp_required") setPassword("");
      setOtp("");
    },
  });
  const err = m.error;
  const code = err instanceof ApiError ? err.code : null;
  const needOtp = otpAsked;
  const known = code && (loginCodes as readonly string[]).includes(code) ? (code as (typeof loginCodes)[number]) : null;
  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (login.trim() && password && (!needOtp || otp.trim())) m.mutate(needOtp ? otp.replace(/\s/g, "") : null);
  };
  if (help) return <LoginHelpForm onBack={() => setHelp(false)} />;
  return (
    <div className="flex min-h-screen items-start justify-center px-4 pt-[12vh]">
      <form onSubmit={submit} className="w-full max-w-sm rounded-2xl border border-line bg-paper p-8 shadow-sm">
        <div className="flex items-start justify-between">
          <Logo />
          <LanguagePicker />
        </div>
        <h1 className="mt-5 text-xl font-semibold">{t("login.title")}</h1>
        <p className="mb-6 text-sm text-muted">{t("login.subtitle")}</p>
        <div className="space-y-4">
          <Field label={t("login.account")} hint={t("login.accountHint")}>
            <Input autoFocus autoComplete="username" value={login} onChange={(e) => setLogin(e.target.value)} />
          </Field>
          <Field label={t("login.password")}>
            <Input type="password" autoComplete="current-password" value={password} onChange={(e) => setPassword(e.target.value)} />
          </Field>
          {needOtp && (
            <Field label={t("login.otp")} hint={t("login.otpHint")}>
              <Input
                inputMode="numeric"
                autoComplete="one-time-code"
                pattern="[0-9 ]*"
                maxLength={7}
                value={otp}
                onChange={(e) => setOtp(e.target.value)}
                className="font-mono tracking-widest"
              />
            </Field>
          )}
          {err && (
            <div role="alert" className="rounded-lg bg-bad/5 px-3 py-2 text-sm text-bad">
              {known ? t(`login.errors.${known}`) : err.message}
            </div>
          )}
          <Button type="submit" variant="primary" className="w-full py-2.5" disabled={m.isPending || !login.trim() || !password || (needOtp && !otp.trim())}>
            {m.isPending ? t("login.signingIn") : t("login.submit")}
          </Button>
          {setup.data?.oidc != null && (
            <>
              <Button type="button" className="w-full py-2.5" onClick={() => window.location.assign("/api/auth/oidc/start")}>
                {setup.data.oidc || t("login.oidc.button")}
              </Button>
              {oidcError && (
                <div role="alert" className="rounded-lg bg-bad/5 px-3 py-2 text-sm text-bad">
                  {t(`login.oidc.errors.${oidcError}`)}
                </div>
              )}
            </>
          )}
          {setup.data?.kerberos && (
            <>
              <Button type="button" className="w-full py-2.5" disabled={domain.isPending} onClick={() => domain.mutate()}>
                {domain.isPending ? t("login.signingIn") : t("login.kerberos.button")}
              </Button>
              {domain.error && (
                <div role="alert" className="rounded-lg bg-bad/5 px-3 py-2 text-sm text-bad">
                  {domainKnown ? t(`login.kerberos.errors.${domainKnown}`) : domain.error.message}
                </div>
              )}
            </>
          )}
        </div>
        {helpdesk && (
          <p className="mt-6 border-t border-line pt-4 text-xs text-muted">
            <button type="button" className="text-accent hover:underline" onClick={() => setHelp(true)}>
              {t("login.cantLogin")}
            </button>
          </p>
        )}
      </form>
    </div>
  );
}

/** Выбор языка до входа: запоминается в этом браузере. */
export function LanguagePicker() {
  const { t, i18n } = useTranslation();
  return (
    <select
      aria-label={t("nav.language")}
      value={i18n.language}
      onChange={(e) => setLanguage(asLang(e.target.value) ?? "ru")}
      className="rounded border border-line bg-paper px-1 py-0.5 text-xs text-muted"
    >
      {LANGUAGES.map(([code, name]) => (
        <option key={code} value={code}>
          {name}
        </option>
      ))}
    </select>
  );
}

/** Логотип и название организации (мастер установки); без них — знак Pult. */
export function Logo() {
  const org = useOrg().data;
  const mark = (
    <div className="text-2xl font-bold tracking-tight">
      pult<span className="text-accent">·</span>
    </div>
  );
  if (!org?.logo && !org?.name) return mark;
  return (
    <div className="flex min-w-0 items-center gap-2">
      {org.logo && <img src={`/api/org/logo?v=${org.logo}`} alt="" className="h-9 max-w-28 shrink-0 object-contain" />}
      {org.name ? <span className="min-w-0 truncate text-lg font-semibold leading-tight">{org.name}</span> : !org.logo && mark}
    </div>
  );
}

/** Вошедший пользователь или форма входа; при потере связи с доменом — экран ожидания. */
export function RequireAuth({ children }: { children: (me: Me) => ReactNode }) {
  const { t } = useTranslation();
  const me = useMe();
  const setup = useQuery({ queryKey: ["setup"], queryFn: () => ok<SetupStatus>(api.GET("/api/setup/status")), retry: false, enabled: !me.data });
  if (me.isPending || (!me.data && setup.isPending)) return <Spinner />;
  if (me.error instanceof ApiError && me.error.code === "ad_unavailable") {
    return (
      <div className="mx-auto mt-[15vh] max-w-md rounded-2xl border border-line bg-paper p-8 text-center">
        <h1 className="mb-2 text-lg font-semibold">{t("outage.title")}</h1>
        <p className="mb-4 text-sm text-muted">{t("outage.text")}</p>
        <Button onClick={() => void me.refetch()}>{t("common.retry")}</Button>
      </div>
    );
  }
  if (!me.data && setup.data?.needed) return <SetupWizard status={setup.data} />;
  if (!me.data) return <LoginPage />;
  return <>{children(me.data)}</>;
}

