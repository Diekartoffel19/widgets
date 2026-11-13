# Desktop Widgets

Kleine, frei gestaltbare Widgets direkt auf dem Desktop – mit einem Editor zum
Anlegen, Gestalten und Verschieben. Geschrieben in Rust mit
[egui/eframe](https://github.com/emilk/egui).

Entwickelt und getestet unter Windows 11. Linux (X11/Wayland) wird mitgebaut,
aber das Anheften an den Desktop gibt es nur unter Windows.

## Widgets

| Typ       | Inhalt                                                        |
|-----------|---------------------------------------------------------------|
| Uhr       | Uhrzeit mit frei wählbarem Format (`%H:%M`, `%H:%M:%S`, …)    |
| Datum     | Datum mit deutschem Wochentag und Monatsnamen                 |
| Text      | Beliebiger, auch mehrzeiliger Text                            |
| System    | CPU- und RAM-Auslastung, jede Sekunde aktualisiert            |
| Countdown | Tage bis zu einem Zieldatum (`JJJJ-MM-TT`)                    |
| Zeichnung | Freihand-Zeichnung mit der Maus                               |

## Bauen und starten

Voraussetzung: [Rust](https://rustup.rs) (Edition 2024, also Rust 1.85 oder neuer).

```sh
cargo run --release
```

Die fertige Datei liegt danach unter `target/release/desktop-widgets.exe`.

## Bedienung

- **+ Neu** legt ein Widget an, die Liste links wählt eines aus.
- **Löschen** fragt einmal nach („Wirklich löschen“).
- **Bearbeiten: an** – Widgets liegen über allen Fenstern und lassen sich mit
  der Maus verschieben.
- **Bearbeiten: aus** – Widgets sind fixiert, liegen hinter allen Fenstern
  direkt über dem Desktop, und Klicks gehen durch sie hindurch.
- Das **X** versteckt nur den Editor, die Widgets laufen weiter. Ein erneuter
  Start der App holt den Editor zurück.
- **Beenden** schließt die App samt Widgets.
- Widgets bleiben bei Win+D („Desktop anzeigen“) sichtbar.

### Aussehen

Pro Widget einstellbar: Schriftgröße, Schriftart, Ausrichtung, Textfarbe,
Textschatten, Hintergrundfarbe mit Deckkraft, Rahmen, Eckenradius,
Innenabstand, Größe und Position.

### Transparenz

- **Deckkraft** (unter „Aussehen“) macht das ganze Widget durchscheinend:
  Text, Hintergrund, Rahmen und Linien (5–100 %).
- **Durchsichtig** neben der Hintergrundfarbe blendet Hintergrund und Rahmen
  aus, nur der Inhalt bleibt. Alternativ den Deckkraft-Regler der
  Hintergrundfarbe auf 0 % stellen.
- **Textschatten** (unter „Schrift“) hält Text ohne Hintergrund auf hellen
  und dunklen Hintergrundbildern lesbar.

### Zeichnung

- Zeichnen: mit der Maus in der Vorschau im Editor oder, bei
  „Bearbeiten: an“, direkt auf dem Widget.
- Verschieben: am blauen Griff oben links ziehen – der Rest der Fläche zeichnet.
- Stiftfarbe und Stiftbreite gelten für neue Linien. **Rückgängig** entfernt
  die letzte Linie, **Leeren** alle.

## Konfiguration

Alle Widgets werden automatisch gespeichert, unter Windows in

```
%APPDATA%\desktop-widgets\widgets.json
```

Die Datei löschen setzt die App auf die Standard-Widgets zurück. Ist sie
unlesbar, legt die App sie als `widgets.json.defekt` beiseite, statt sie zu
überschreiben.

## Technische Hinweise

- Jedes Widget ist ein eigenes, rahmenloses, transparentes Fenster (egui
  Viewport), das dem Desktop-Fenster (`Progman`) zugeordnet wird.
- Transparenz unter Windows: Manche Treiber (z. B. Intel Arc) melden kein
  `WGL_TRANSPARENT_ARB`, woraufhin glutin-winit die Transparenz der
  Widget-Fenster abschaltet. Die App ruft deshalb `DwmEnableBlurBehindWindow`
  selbst auf (`enable_alpha` in `src/main.rs`).
- Gesperrte Widgets zeichnen nur zum Sekundenwechsel neu; CPU- und RAM-Werte
  werden nur gelesen, wenn ein System-Widget existiert.
- Speicher: Der Schrift-Atlas ist auf 2048 px Breite begrenzt (egui nimmt
  sonst bis zu 16384 px), fertige Freihand-Linien werden ausgedünnt, und der
  versteckte Editor baut keine Oberfläche auf.
