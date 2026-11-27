#Requires -Version 5.1
<#
.SYNOPSIS
Collects this machine's firmware tables for the Origin OS project (TASKS item 169; owner items
381 and 382).

.DESCRIPTION
Reads, and only reads:
  - the raw ACPI tables Windows exposes, through EnumSystemFirmwareTables and
    GetSystemFirmwareTable (provider 'ACPI');
  - the raw SMBIOS data (provider 'RSMB');
  - a summary of the processor, board, firmware, memory, disks and Windows, through CIM, with no
    serial numbers and no names of the machine or its users;
  - whether the firmware is UEFI and whether Secure Boot is on.

It writes them, with a manifest of SHA-256 digests and each ACPI table's checksum, into a new
folder and a zip file of it. It downloads nothing, installs nothing and changes no setting.

PRIVACY: the SMBIOS data, and some ACPI tables, hold this machine's serial numbers and other
identifiers. Send the zip only to the person who asked you for it.

Windows returns one table for each ACPI signature, so a machine with several SSDTs gives only the
first. That is a limit of Windows' interface, recorded in the manifest.

HOW TO RUN: open PowerShell (no administrator rights are needed) in the folder holding this file
and run

    powershell -NoProfile -ExecutionPolicy Bypass -File .\collect-firmware.ps1

"-ExecutionPolicy Bypass" applies to that one run only and changes no setting. The zip is written
to the desktop unless -OutputDirectory names another folder.

.PARAMETER OutputDirectory
The folder in which the result folder and its zip are made. The desktop by default.

.PARAMETER Strict
Fail (exit code 1) unless a FADT, a DSDT and SMBIOS were read. For the project's CI job.
#>
[CmdletBinding()]
param(
    [string] $OutputDirectory = [Environment]::GetFolderPath('Desktop'),
    [switch] $Strict
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class OriginFirmware
{
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern uint EnumSystemFirmwareTables(uint provider, byte[] buffer, uint size);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern uint GetSystemFirmwareTable(uint provider, uint id, byte[] buffer, uint size);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetFirmwareType(out uint type);

    // A provider's signature, as the API takes it: the first character in the high byte.
    public static uint Provider(string four)
    {
        if (four.Length != 4) throw new ArgumentException("a signature has four characters");
        return ((uint)four[0] << 24) | ((uint)four[1] << 16) | ((uint)four[2] << 8) | (uint)four[3];
    }

    // A table's identifier as Windows lists it: its four bytes in memory order.
    public static uint TableId(string four)
    {
        if (four.Length != 4) throw new ArgumentException("a signature has four characters");
        return (uint)four[0] | ((uint)four[1] << 8) | ((uint)four[2] << 16) | ((uint)four[3] << 24);
    }

    public static string TableName(uint id)
    {
        char[] c = new char[4];
        for (int i = 0; i < 4; i++) c[i] = (char)((id >> (8 * i)) & 0xFF);
        return new string(c);
    }

    public static uint[] List(uint provider)
    {
        uint size = EnumSystemFirmwareTables(provider, null, 0);
        if (size == 0) return new uint[0];
        byte[] buffer = new byte[size];
        uint got = EnumSystemFirmwareTables(provider, buffer, size);
        if (got == 0 || got > size) throw new InvalidOperationException(
            "EnumSystemFirmwareTables failed, error " + Marshal.GetLastWin32Error());
        uint[] ids = new uint[got / 4];
        for (int i = 0; i < ids.Length; i++) ids[i] = BitConverter.ToUInt32(buffer, 4 * i);
        return ids;
    }

    // The table's bytes, or null if Windows does not give it.
    public static byte[] Get(uint provider, uint id)
    {
        uint size = GetSystemFirmwareTable(provider, id, null, 0);
        if (size == 0) return null;
        byte[] buffer = new byte[size];
        uint got = GetSystemFirmwareTable(provider, id, buffer, size);
        if (got == 0 || got > size) return null;
        if (got == size) return buffer;
        byte[] shorter = new byte[got];
        Array.Copy(buffer, shorter, got);
        return shorter;
    }

    // "UEFI", "BIOS" or "unknown".
    public static string FirmwareType()
    {
        uint type;
        if (!GetFirmwareType(out type)) return "unknown";
        if (type == 1) return "BIOS";
        if (type == 2) return "UEFI";
        return "unknown (" + type + ")";
    }
}
'@

function Get-AcpiChecksumOk([byte[]] $Bytes) {
    $sum = 0
    foreach ($b in $Bytes) { $sum = ($sum + $b) -band 0xFF }
    return ($sum -eq 0)
}

function Get-AcpiHeaderLength([byte[]] $Bytes) {
    if ($Bytes.Length -lt 8) { return -1 }
    return [BitConverter]::ToUInt32($Bytes, 4)
}

function Get-Sha256([string] $Path) {
    return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

$stamp = (Get-Date).ToUniversalTime().ToString('yyyyMMdd-HHmmss')
$name = "OriginFirmware-$stamp"
$folder = Join-Path $OutputDirectory $name
$acpiFolder = Join-Path $folder 'acpi'
New-Item -ItemType Directory -Path $acpiFolder -Force | Out-Null

$manifest = New-Object System.Collections.Generic.List[string]
$manifest.Add("Origin OS firmware tables, collected $stamp UTC by collect-firmware.ps1 (TASKS item 169).")
$manifest.Add("Windows returns one ACPI table for each signature; SSDTs past the first are not here.")
$manifest.Add("")

# ACPI tables: every signature Windows lists, each once, and the DSDT and FADT by name if the
# list leaves them out.
$acpi = [OriginFirmware]::Provider('ACPI')
$listed = [OriginFirmware]::List($acpi)
$counts = @{}
foreach ($id in $listed) {
    $sig = [OriginFirmware]::TableName($id)
    if ($counts.ContainsKey($sig)) { $counts[$sig] += 1 } else { $counts[$sig] = 1 }
}
$wanted = New-Object 'System.Collections.Generic.List[System.UInt32]'
foreach ($id in $listed) { if (-not $wanted.Contains($id)) { $wanted.Add($id) } }
foreach ($sig in @('FACP', 'DSDT')) {
    $id = [OriginFirmware]::TableId($sig)
    if (-not $wanted.Contains($id)) { $wanted.Add($id) }
}

$manifest.Add("ACPI signatures Windows lists, with how many times each is listed:")
foreach ($sig in ($counts.Keys | Sort-Object)) { $manifest.Add("  $sig x$($counts[$sig])") }
$manifest.Add("")
$manifest.Add("ACPI tables read (file, bytes, header length, checksum, SHA-256):")

$read = @{}
foreach ($id in $wanted) {
    $sig = [OriginFirmware]::TableName($id)
    $bytes = [OriginFirmware]::Get($acpi, $id)
    if ($null -eq $bytes) {
        $manifest.Add("  $sig : not given by Windows")
        continue
    }
    $safe = ($sig -replace '[^A-Za-z0-9]', '_')
    $file = Join-Path $acpiFolder "$safe.bin"
    [System.IO.File]::WriteAllBytes($file, $bytes)
    $read[$sig] = $true
    $checksum = if (Get-AcpiChecksumOk $bytes) { 'checksum ok' } else { 'CHECKSUM BAD' }
    $manifest.Add("  acpi/$safe.bin $($bytes.Length) $(Get-AcpiHeaderLength $bytes) $checksum $(Get-Sha256 $file)")
}
$manifest.Add("")

# SMBIOS: the RawSMBIOSData structure as Windows gives it (calling method, major, minor, DMI
# revision, length, then the table).
$smbios = [OriginFirmware]::Get([OriginFirmware]::Provider('RSMB'), 0)
$smbiosRead = $false
if ($null -ne $smbios -and $smbios.Length -ge 8) {
    $file = Join-Path $folder 'smbios.bin'
    [System.IO.File]::WriteAllBytes($file, $smbios)
    $smbiosRead = $true
    $manifest.Add("SMBIOS $($smbios[1]).$($smbios[2]): smbios.bin $($smbios.Length) bytes $(Get-Sha256 $file)")
} else {
    $manifest.Add("SMBIOS: not given by Windows")
}
$manifest.Add("")

# A summary without serial numbers and without the names of the machine or its users.
function Get-Selected([string] $Class, [string[]] $Properties) {
    try {
        return @(Get-CimInstance -ClassName $Class | Select-Object -Property $Properties)
    } catch {
        return @("not read: $($_.Exception.Message)")
    }
}

$secureBoot = 'unknown'
try {
    $state = Get-ItemProperty -LiteralPath 'HKLM:\SYSTEM\CurrentControlSet\Control\SecureBoot\State' -Name 'UEFISecureBootEnabled'
    $secureBoot = if ($state.UEFISecureBootEnabled -eq 1) { 'on' } else { 'off' }
} catch {
    $secureBoot = 'unknown (not readable)'
}

$summary = [ordered]@{
    collected_utc  = $stamp
    firmware_type  = [OriginFirmware]::FirmwareType()
    secure_boot    = $secureBoot
    processor      = Get-Selected 'Win32_Processor' @('Manufacturer', 'Name', 'Caption', 'ProcessorId', 'NumberOfCores', 'NumberOfLogicalProcessors', 'MaxClockSpeed', 'L2CacheSize', 'L3CacheSize')
    system         = Get-Selected 'Win32_ComputerSystem' @('Manufacturer', 'Model', 'SystemType', 'TotalPhysicalMemory')
    board          = Get-Selected 'Win32_BaseBoard' @('Manufacturer', 'Product', 'Version')
    firmware       = Get-Selected 'Win32_BIOS' @('Manufacturer', 'SMBIOSBIOSVersion', 'ReleaseDate', 'SMBIOSMajorVersion', 'SMBIOSMinorVersion')
    memory         = Get-Selected 'Win32_PhysicalMemory' @('Manufacturer', 'PartNumber', 'Capacity', 'Speed', 'ConfiguredClockSpeed', 'DeviceLocator')
    disks          = Get-Selected 'Win32_DiskDrive' @('Model', 'InterfaceType', 'MediaType', 'Size', 'FirmwareRevision')
    windows        = Get-Selected 'Win32_OperatingSystem' @('Caption', 'Version', 'BuildNumber', 'OSArchitecture')
}
$summaryFile = Join-Path $folder 'summary.json'
$summary | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $summaryFile -Encoding ASCII
$manifest.Add("summary.json $(Get-Sha256 $summaryFile)")

$manifestFile = Join-Path $folder 'manifest.txt'
$manifest | Set-Content -LiteralPath $manifestFile -Encoding ASCII

$zip = Join-Path $OutputDirectory "$name.zip"
Compress-Archive -Path (Join-Path $folder '*') -DestinationPath $zip -Force

Write-Host ""
Write-Host "Done. Read $($read.Count) ACPI tables and SMBIOS: $smbiosRead."
Write-Host "The result is $zip"
Write-Host "It holds this machine's serial numbers; send it only to the person who asked for it."

if ($Strict) {
    $missing = @()
    if (-not $read.ContainsKey('FACP')) { $missing += 'FACP' }
    if (-not $read.ContainsKey('DSDT')) { $missing += 'DSDT' }
    if (-not $smbiosRead) { $missing += 'SMBIOS' }
    if ($missing.Count -gt 0) {
        [Console]::Error.WriteLine("collect-firmware: missing: $($missing -join ', ')")
        exit 1
    }
}
