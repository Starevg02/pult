# Служебные учётки Pult в AD (docs/ad-setup.md, раздел «Служебные учётки»).
# Запускается с ПК с модулем ActiveDirectory под обычной учёткой: права администратора домена
# берутся из окна запроса учётных данных (-Credential), пароли учёток вводятся скрыто.
# Повторный запуск безопасен: существующие OU, учётки и SPN пропускаются.
#
#   powershell -ExecutionPolicy Bypass -File deploy\ad\pult-service-accounts.ps1 -Admin CORP\admin -SpnHost pult.corp.example.local
param(
    # Учётка администратора домена (DOMAIN\user); пароль спросит окно Windows.
    [Parameter(Mandatory)][string]$Admin,
    [string]$OuName = "Service Accounts",
    # Полное DNS-имя портала: для него регистрируется SPN HTTP/<имя> (вход через домен).
    [Parameter(Mandatory)][string]$SpnHost
)
$ErrorActionPreference = "Stop"
Import-Module ActiveDirectory

$cred = Get-Credential -UserName $Admin -Message "Администратор домена для создания служебных учёток Pult"
$domain = Get-ADDomain -Credential $cred
$ou = "OU=$OuName,$($domain.DistinguishedName)"

if (-not (Get-ADOrganizationalUnit -Filter "DistinguishedName -eq '$ou'" -Credential $cred)) {
    New-ADOrganizationalUnit -Name $OuName -Path $domain.DistinguishedName -Credential $cred `
        -Description "Служебные учётки приложений" -ProtectedFromAccidentalDeletion $true
    Write-Host "OU создана: $ou"
} else {
    Write-Host "OU уже есть: $ou"
}

function Read-ServicePassword([string]$name) {
    while ($true) {
        $p1 = Read-Host -AsSecureString "Пароль $name (не короче 20 символов)"
        $p2 = Read-Host -AsSecureString "Повторите пароль $name"
        $plain1 = [Runtime.InteropServices.Marshal]::PtrToStringBSTR([Runtime.InteropServices.Marshal]::SecureStringToBSTR($p1))
        $plain2 = [Runtime.InteropServices.Marshal]::PtrToStringBSTR([Runtime.InteropServices.Marshal]::SecureStringToBSTR($p2))
        $ok = ($plain1 -ceq $plain2) -and ($plain1.Length -ge 20)
        $plain1 = $null; $plain2 = $null
        if ($ok) { return $p1 }
        Write-Host "Пароли не совпали или короче 20 символов — ещё раз." -ForegroundColor Yellow
    }
}

$accounts = @(
    @{ Name = "svc-pult-http"; Description = "Pult: Kerberos (SPN HTTP)" },
    @{ Name = "svc-pult-ldap"; Description = "Pult: чтение каталога AD" },
    @{ Name = "svc-pult-sql";  Description = "Pult: чтение PhonebookDB (Windows-вход в SQL Server)" }
)

foreach ($a in $accounts) {
    $existing = Get-ADUser -Filter "SamAccountName -eq '$($a.Name)'" -Credential $cred
    if ($existing) {
        Write-Host "Учётка уже есть: $($a.Name) ($($existing.DistinguishedName))"
        continue
    }
    $pass = Read-ServicePassword $a.Name
    New-ADUser -Name $a.Name -SamAccountName $a.Name -UserPrincipalName "$($a.Name)@$($domain.DNSRoot)" `
        -Path $ou -Description $a.Description -AccountPassword $pass -Enabled $true `
        -PasswordNeverExpires $true -CannotChangePassword $true -Credential $cred
    # Только AES: без RC4 билеты Kerberos для этих учёток не подобрать по слабому шифру.
    Set-ADUser $a.Name -KerberosEncryptionType AES128, AES256 -Credential $cred
    Write-Host "Создана: $($a.Name)"
}

$spn = "HTTP/$SpnHost"
$owner = Get-ADObject -LDAPFilter "(servicePrincipalName=$spn)" -Properties servicePrincipalName -Credential $cred
if ($owner -and $owner.Name -ne "svc-pult-http") {
    throw "SPN $spn уже выдан объекту $($owner.DistinguishedName) — дубль сломает Kerberos, разберитесь вручную"
} elseif ($owner) {
    Write-Host "SPN уже есть: $spn → svc-pult-http"
} else {
    Set-ADUser svc-pult-http -ServicePrincipalNames @{ Add = $spn } -Credential $cred
    Write-Host "SPN выдан: $spn → svc-pult-http"
}

Write-Host "`nИтог:"
Get-ADUser -Filter "SamAccountName -like 'svc-pult-*'" -Properties servicePrincipalName, msDS-SupportedEncryptionTypes, msDS-KeyVersionNumber -Credential $cred |
    Sort-Object SamAccountName |
    Format-Table SamAccountName, Enabled, @{ n = "SPN"; e = { $_.servicePrincipalName -join ", " } },
        @{ n = "EncTypes"; e = { $_.'msDS-SupportedEncryptionTypes' } }, @{ n = "kvno"; e = { $_.'msDS-KeyVersionNumber' } } -AutoSize
