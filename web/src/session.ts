// Текущий пользователь и выход (данные; компоненты входа — в auth.tsx).
import { useMutation, useQuery } from "@tanstack/react-query";
import { api, ok, type Me, type Modules, type OrgSettings } from "./api/client";
import { currentEndpoint, unsubscribeBrowser } from "./push";

export function useMe() {
  return useQuery({
    queryKey: ["me"],
    queryFn: () => ok<Me>(api.GET("/api/me")),
    retry: false,
    staleTime: 60_000,
  });
}

/** Есть ли у пользователя разрешение (суперадминистратор — всё). Только чтобы прятать кнопки. */
export function can(me: Me | undefined, permission: string): boolean {
  return !!me && (me.is_superadmin || me.permissions.includes(permission));
}

export function useLogout() {
  return useMutation({
    mutationFn: async () => {
      // После выхода устройство не должно получать уведомления этого пользователя (компьютер общий).
      try {
        const endpoint = await currentEndpoint();
        if (endpoint) {
          await api.DELETE("/api/me/push", { body: { endpoint } });
          await unsubscribeBrowser();
        }
      } catch {
        // подписки нет или браузер не дал её снять — выход важнее
      }
      return ok(api.POST("/api/auth/logout"));
    },
    // Полная перезагрузка: в памяти вкладки не остаётся данных прошлого пользователя и потока событий.
    // При ошибке (например, CSRF) сеанс жив — перезагрузка сделала бы вид, что выход удался.
    onSuccess: () => {
      try {
        sessionStorage.removeItem("pult.started");
      } catch {
        // хранилище недоступно — стартовая страница просто не откроется сама
      }
      window.location.assign("/");
    },
  });
}

/** Организация: название, логотип, включённые модули (читается и до входа). */
export function useOrg() {
  return useQuery({
    queryKey: ["org"],
    queryFn: () => ok<OrgSettings>(api.GET("/api/org")),
    // Модули включает администратор на другом компьютере: минуты, а не часы.
    staleTime: 60_000,
  });
}

export type ModuleKey = keyof Modules;

/** Включён ли модуль. Пока настройки не загружены — считаем включённым (как сервер по умолчанию). */
export function useModule(m: ModuleKey): boolean {
  return useOrg().data?.modules[m] ?? true;
}
