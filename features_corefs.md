# Anforderungsliste für ein neues Filesystem

> **Hinweis**: Dieses Dokument ist die **fachliche Zieldefinition** — nicht der Umsetzungsstand. Den aktuellen Implementierungsstand mit Status-Markern pro Feature liefert [doc/features.md](doc/features.md); den detaillierten Phasen-Fortschritt [PROJECT_PROGRESS.md](PROJECT_PROGRESS.md).

## 1. Zielbild

Das Dateisystem soll ein modernes, performantes und fehlertolerantes Filesystem sein, das speziell auf folgende Eigenschaften ausgelegt ist:

* hohe Performance bei vielen parallelen Zugriffen
* SSD-Optimierung
* integrierte Versionierung
* Datenintegrität und Self-Healing
* Wiederherstellbarkeit gelöschter Dateien
* Clusterfähigkeit und Synchronisationsstatus
* semantische Erweiterbarkeit durch Tags, Attribute und Inhaltsindexierung
* native Nutzbarkeit als Standard-Dateisystem des eigenen Betriebssystems
* optionale Plattform- und Kompatibilitätsadapter für Fremdsysteme

Es soll gleichzeitig **leichtgewichtig**, **transaktionssicher** und **erweiterbar** sein.

---

## 2. Funktionale Anforderungen

### 2.1 Grundlegende Dateisystem-Funktionen

* Das Dateisystem muss **case-sensitiv** arbeiten.
* Es muss **symbolische Links** unterstützen.
* Es muss **atomare Rename- und Move-Operationen** unterstützen, damit Umbenennungen und Verschiebungen auch bei Abstürzen konsistent bleiben.
* Es muss **beliebige Verzeichnistiefe** ermöglichen.
* Es muss **sehr große oder praktisch unbeschränkte Pfadlängen** unterstützen.
* Es muss **beliebig viele Dateien pro Verzeichnis** verwalten können.
* Die Auflösung von Dateien und Verzeichnissen soll möglichst effizient sein, idealerweise in **O(1)** oder andernfalls in gut skalierbaren Strukturen.
* Es soll über **kompatible Semantik- und API-Adapter** verfügen können, damit bestehende Werkzeuge und Anwendungen auf Fremdplattformen angebunden werden können.
* Es muss neben klassischen Unix-Rechten auch **ACLs und erweiterte Zugriffsrechte** unterstützen.

### 2.2 Speicherverwaltung

* Das Dateisystem muss **Journaling** unterstützen.
* Es soll zusätzlich **Copy-on-Write** unterstützen, sofern dies mit dem Journaling-Konzept vereinbar ist.
* Es soll **transparente Kompression** unterstützen, um Speicherbedarf und I/O-Last zu reduzieren.
* Es muss **interne Deduplizierung** bereitstellen. Priorität: späterer Ausbau nach MVP.
* Es muss Mechanismen zur **Blockoptimierung** besitzen, damit zusammengehörige Blöcke möglichst sequenziell gelesen und geschrieben werden können.
* Es muss **Hot/Cold-Storage-Mechanismen** unterstützen, um häufig und selten genutzte Daten unterschiedlich zu behandeln. Priorität: späterer Ausbau nach MVP.
* Der Benutzer soll nur den **effektiv genutzten Speicherplatz** sehen. Speicher, der intern für Versionierung oder Recovery reserviert wird, soll für ihn weitgehend transparent bleiben. Priorität: späterer Ausbau nach MVP.
* Es muss **Quotas** für Benutzer, Gruppen oder definierte Bereiche unterstützen.
* Es soll **Tiering auf unterschiedliche Speichermedien** unterstützen, wenn mehrere Medienklassen vorhanden sind. Priorität: späterer Ausbau nach MVP.

### 2.3 Versionierung und Time Travel

* Das Dateisystem muss eine **automatische Dateiversionierung** besitzen.
* Es soll **Snapshots auf Verzeichnisebene** bzw. für konsistente Teilbäume unterstützen, damit nicht nur einzelne Dateien, sondern vollständige Zustände gesichert werden können.
* Alte Versionen sollen automatisch aufgelöst oder bereinigt werden können, wenn der freie Speicherplatz knapp wird. Priorität: späterer Ausbau nach MVP.
* Es muss ein natives **Time-Travel-Konzept** geben, sodass frühere Dateistände direkt adressierbar sind, zum Beispiel in einer Form wie `/file.txt@2026-04-01-12:03`. Priorität: späterer Ausbau nach MVP.
* Die Versionierung soll systemweit konsistent und ohne Benutzerinteraktion funktionieren.
* Es soll eine **Backup- und Export-Schnittstelle** für inkrementelle Sicherungen und Replikationsläufe geben.

### 2.4 Löschen und Wiederherstellung

* Beim Löschen einer Datei sollen **Dateiindex und Datenblöcke zunächst erhalten bleiben**, solange ausreichend Speicherplatz vorhanden ist.
* Das System muss eine **spätere Wiederherstellung gelöschter Dateien** ermöglichen.
* Die Wiederherstellung soll über **Systemfunktionen** oder integrierte Werkzeuge möglich sein.
* Es muss zusätzlich Verfahren zum **sicheren Löschen** geben.
* Beim sicheren Löschen müssen sowohl Metadaten als auch Datenblöcke zuverlässig entfernt bzw. genullt werden.

### 2.5 Integrität und Fehlertoleranz

* Das Dateisystem muss **Checksummen für Blöcke und Dateien** führen.
* Es muss Fehler in Daten und Metadaten erkennen können.
* Es muss **Self-Healing** unterstützen, also beschädigte Daten aus redundanten oder replizierten Quellen automatisch reparieren können. Priorität: späterer Ausbau nach MVP.
* Es soll **Online-Scrubbing** unterstützen, um Daten und Metadaten periodisch im laufenden Betrieb zu prüfen. Priorität: späterer Ausbau nach MVP.
* Es muss **transaktionsbasiert** arbeiten, um konsistente Zustände auch nach Abstürzen sicherzustellen.
* Es muss eine klar definierte **fsck- und Recovery-Strategie** für Offline-Prüfung und Notfallreparatur geben.

### 2.6 Metadaten, Tags und semantische Funktionen

* Zu jeder Datei müssen **beliebig viele Tags und Attribute** gespeichert werden können.
* Das Dateisystem soll als **semantisches Dateisystem** ausgelegt werden. Priorität: späterer Ausbau nach MVP.
* Dateien sollen automatisch anhand ihres Inhalts klassifiziert und indexiert werden können. Priorität: späterer Ausbau nach MVP. Beispiele hierfür sind:

  * Texte
  * Bilder
  * Quellcode
* Es soll möglich sein, Inhalte und Metadaten für erweiterte Such- und Organisationsfunktionen zu verwenden.
* Es soll eine **API oder Systemschnittstelle für Metadaten, Tags und semantische Abfragen** geben, damit Anwendungen diese Funktionen gezielt nutzen können.

### 2.7 Sicherheit

* Das Dateisystem soll **Verschlüsselung ruhender Daten** unterstützen.
* Die Verschlüsselung soll möglichst flexibel auf Ebene von Volume, Verzeichnis oder Datei anwendbar sein.
* Schlüsselverwaltung und Zugriffskontrolle müssen so gestaltet sein, dass Wiederherstellung und Administration praktikabel bleiben.

---

## 3. Nichtfunktionale Anforderungen

### 3.1 Performance

* Das Dateisystem muss für **viele parallele Zugriffe** optimiert sein.
* Es soll **maximale Geschwindigkeit** erreichen.
* Es soll möglichst **leichtgewichtig** sein und unnötigen Overhead vermeiden.
* Die Metadatenverwaltung muss effizient und skalierbar ausgelegt sein.
* Häufig genutzte Blöcke sollen markiert werden können, um **Hot Paths** zu erkennen und Optimierungen darauf aufzubauen. Priorität: späterer Ausbau nach MVP.
* Es soll **Hintergrund-Rebalancing** unterstützen, um Datenlayout und Performance ohne längere Ausfallzeiten zu verbessern. Priorität: späterer Ausbau nach MVP.

### 3.2 SSD-Optimierung

* Das Dateisystem muss für **SSDs** optimiert sein.
* Es muss Funktionen wie **TRIM/Discard-Unterstützung** bereitstellen.
* Schreibmuster und Blockplatzierung sollen SSD-freundlich gestaltet sein.

### 3.3 Clusterfähigkeit

* Das Dateisystem muss **clusterfähig** sein. Priorität: späterer Ausbau nach MVP.
* Es muss eindeutig nachvollziehbar sein, welche Blöcke oder Datenbereiche bereits synchronisiert wurden. Priorität: späterer Ausbau nach MVP.
* Es soll Mechanismen für konsistente Synchronisation und Replikation zwischen Knoten geben. Priorität: späterer Ausbau nach MVP.

### 3.4 Plattformunterstützung

* Es muss eine **native Systemintegration** für das eigene Betriebssystem geben, damit CoreFS dort als primäres Dateisystem genutzt werden kann.
* Es sollen zusätzlich **optionale Plattformadapter** für Fremdsysteme möglich sein, ohne das Dateisystem konzeptionell an eine einzelne Plattform zu binden.
* Es müssen Werkzeuge zum **Formatieren** und Verwalten des Dateisystems bereitgestellt werden.
* Es soll **konfigurierbare Mount-Optionen und Betriebsrichtlinien** geben, um Integrität, Performance, Kompression oder Recovery-Verhalten steuern zu können.

### 3.5 Dokumentation

* Es muss eine **saubere und vollständige Dokumentation** vorhanden sein.
* Die Dokumentation soll mindestens enthalten:

  * Architektur
  * On-Disk-Format
  * API-/Treiberverhalten
  * Recovery- und Wiederherstellungsmechanismen
  * Administrations- und Formatierungswerkzeuge

---

## 4. Technische Leitprinzipien

Das Dateisystem soll sich an folgenden Prinzipien orientieren:

* **Konsistenz vor Komplexität**
* **Wiederherstellbarkeit vor endgültigem Löschen**
* **Transaktionssicherheit**
* **Skalierbarkeit bei Dateianzahl, Ordnertiefe und Parallelität**
* **Integrität durch Checksummen und Self-Healing**
* **Performance durch optimierte Blockplatzierung und Hot-Path-Erkennung**
* **Erweiterbarkeit durch semantische Metadaten und Inhaltsbewusstsein**

---

## 5. Offene Architekturfragen

Diese Punkte sollten vor der eigentlichen Implementierung noch sauber entschieden werden:

* Wie werden **Journaling** und **Copy-on-Write** kombiniert?
* Wie werden **Snapshots**, Versionierung und inkrementelle Backups intern voneinander abgegrenzt?
* Wie genau funktioniert die **interne Deduplizierung** ohne starken Performanceverlust?
* Wie wird **Kompression** gewählt und auf welchen Ebenen wird sie aktiviert?
* Wie werden **ACLs**, Verschlüsselung und Wiederherstellung zusammengedacht?
* Wie werden **Quotas** und semantische Gruppen wie Tags oder Projekte miteinander verknüpft?
* Wie wird der **versteckte Speicherverbrauch** für Versionierung intern bilanziert?
* Wie wird die **Wiederherstellung gelöschter Dateien** technisch umgesetzt?
* Wie wird **sicheres Löschen auf SSDs** verlässlich definiert, wenn physisches Überschreiben nicht immer garantiert ist?
* Wie wird die **Cluster-Synchronisation** modelliert?
* Wie tief soll die **semantische Inhaltsanalyse** im Dateisystem selbst verankert sein?
* Wie wird die **Schlüsselverwaltung** für Verschlüsselung umgesetzt?
* Welche Datenstrukturen werden für Verzeichnisse, Metadaten und Blockindizes verwendet, damit die Lookup-Zeiten skalierbar bleiben?
* Wie wird ein freier **FS-Type-Identifier** oder eine eindeutige Kennung vergeben?

---

## 6. Priorisierte MVP-Anforderungen

Für eine erste implementierbare Version würde ich diese Punkte als **MVP** priorisieren:

### Muss in Version 1 enthalten sein

* Journaling
* case-sensitives Verhalten
* symbolische Links
* atomare Rename- und Move-Operationen
* große Pfadlängen und tiefe Verzeichnisbäume
* viele Dateien pro Verzeichnis
* native Integration in das eigene Betriebssystem
* optional aktivierbare Kompatibilitätsadapter
* ACLs und erweiterte Zugriffsrechte
* Checksummen für Daten und Metadaten
* Copy-on-Write oder klar definiertes Transaktionsmodell
* transparente Kompression
* Formatierungs- und Verwaltungswerkzeuge
* konfigurierbare Mount-Optionen
* SSD-Optimierung
* effiziente Metadatenverwaltung
* Quotas
* Wiederherstellung gelöschter Dateien
* automatische Versionierung in Grundform
* Snapshots auf Verzeichnisebene
* Verschlüsselung ruhender Daten
* definierte fsck- und Recovery-Strategie
* API oder Systemschnittstelle für Metadaten und Tags

Die Anforderungen mit Priorität `späterer Ausbau nach MVP` sind in den jeweiligen Fachabschnitten direkt markiert, damit sie nicht von ihrer inhaltlichen Einordnung getrennt sind.

---

## 7. Kurzfassung in einem Satz

**Ein transaktionssicheres, SSD-optimiertes, versionierendes und semantisch erweiterbares Dateisystem für das eigene Betriebssystem mit Recovery, Self-Healing, Clusterfähigkeit und hoher Parallel-Performance.**
