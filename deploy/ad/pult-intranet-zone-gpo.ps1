# GPO: адрес Pult — в зоне «Местная интрасеть», чтобы браузеры (Edge, Chrome) сами отправляли билет Kerberos
# (Kerberos; docs/ad-setup.md, раздел 4).
# Запускать в PowerShell под администратором домена (модуль GroupPolicy). Повторный запуск безопасен.
#
#   .\deploy\ad\pult-intranet-zone-gpo.ps1 -Sites https://pult.corp.example.local   # создать GPO и привязать к домену
#   .\deploy\ad\pult-intranet-zone-gpo.ps1 -Remove                                  # отвязать и удалить
#
# Политика — «Список назначений зоны для веб-сайтов» (Конфигурация компьютера): действует для всех
# пользователей ПК, другие сайты и списки зон из других GPO не затрагивает (своё значение реестра).
param(
    # Адреса портала (https://…); при -Remove не нужны.
    [string[]]$Sites = @(),
    [string]$Name = "Pult — местная интрасеть",
    [switch]$Remove
)
$ErrorActionPreference = "Stop"
Import-Module GroupPolicy
$domain = (Get-ADDomain).DistinguishedName
$keys = @(
    # Включить сам список назначений и задать пары «сайт = зона» (1 — местная интрасеть).
    "HKLM\Software\Policies\Microsoft\Windows\CurrentVersion\Internet Settings\ZoneMapKey"
)

if ($Remove) {
    if (Get-GPO -Name $Name -ErrorAction SilentlyContinue) {
        Remove-GPLink -Name $Name -Target $domain -ErrorAction SilentlyContinue | Out-Null
        Remove-GPO -Name $Name
        Write-Host "GPO «$Name» удалена."
    } else {
        Write-Host "GPO «$Name» нет."
    }
    return
}

if (-not $Sites) { throw "Укажите адрес портала: -Sites https://pult.corp.example.local" }
foreach ($site in $Sites) {
    if ($site -notmatch '^https://[a-z0-9.-]+$') { throw "Адрес «$site» — нужен https://имя без пути" }
}
$gpo = Get-GPO -Name $Name -ErrorAction SilentlyContinue
if (-not $gpo) {
    $gpo = New-GPO -Name $Name -Comment "Pult: Kerberos. Адрес портала — в зоне «Местная интрасеть»."
    Write-Host "GPO создана: $Name"
}
foreach ($site in $Sites) {
    Set-GPRegistryValue -Name $Name -Key $keys[0] -ValueName $site -Type String -Value "1" | Out-Null
    Write-Host "Зона «Местная интрасеть»: $site"
}
# Пользовательская часть GPO не нужна — только компьютерная.
$gpo.GpoStatus = "UserSettingsDisabled"

$linked = (Get-GPInheritance -Target $domain).GpoLinks.DisplayName -contains $Name
if (-not $linked) {
    New-GPLink -Name $Name -Target $domain -LinkEnabled Yes | Out-Null
    Write-Host "Привязана к домену: $domain"
} else {
    Write-Host "Уже привязана к домену."
}
Write-Host "`nПК применят политику при обновлении (до 90 минут) или сразу: gpupdate /target:computer /force"
Get-GPRegistryValue -Name $Name -Key $keys[0] | Format-Table ValueName, Value -AutoSize
