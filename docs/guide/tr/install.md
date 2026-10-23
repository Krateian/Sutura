# Kurulum ve gereksinimler

[← README'ye dön](../../../README.tr.md) · [English](../install.md)

## Gereksinimler

Linux:

* `python3` (>= 3.11), venv desteğiyle, onarım venv'i için
* `sutura_geom` kaynaktan derlenecekse (hazır wheel yoksa): Rust >= 1.89,
  cmake ve C++20 derleyicisi (g++/clang++). Sürüm kurulumları hazır wheel
  indirir ve bunlara ihtiyaç duymaz.
* Dolphin servis menüsü için KDE Plasma (isteğe bağlı; CLI ve GUI her yerde
  çalışır)

macOS (Apple Silicon / Intel):

* Homebrew ve Miniforge (conda). macOS, PySide6 (Qt) ve geri kalan yerel yığının
  tutarlı biçimde kurulması için tek bir conda ortamı kullanır
  (`install-macos.sh`); Linux'un yalnızca pip kullanan akışı yerine.
* Python 3.11'i conda ile kurun (`install-macos.sh` bunu otomatik yapar).

Python 3.11 yalnızca isteğe bağlı fTetWild eki için gerekir
(`SUTURA_WITH_FTETWILD=1`); normal kurulum kullanmaz.

* Arch: Python 3.11 resmi depolarda bulunmaz; `install.sh` eksik olduğunda otomatik olarak taşınabilir bir çalışma zamanı indirir (`SUTURA_NO_PYTHON_DOWNLOAD=1` ile devre dışı bırakılabilir)
* Debian / Ubuntu 22.04+: `sudo apt install python3.11 python3.11-venv`
* Fedora: `sudo dnf install python3.11`

## Kurulum

### Linux

Tek satır (en güncel `main`'i indirir ve kurar):

```sh
curl -fsSL https://raw.githubusercontent.com/Krateian/Sutura/main/install.sh | bash
```

Veya bir klondan:

```sh
git clone https://github.com/Krateian/Sutura.git
cd Sutura
./install.sh
```

Bu, `~/.local/share/sutura` altında ana sanal ortamı oluşturur (isteğe bağlı
fTetWild eki istendiğinde ayrıca bir `venv311`), CLI sarmalayıcıyı
`~/.local/bin/sutura`'ya kurar, hicolor uygulama ikonlarını kurar ve Dolphin
servis menüsünü kaydeder. Yeniden çalıştırmak güvenlidir.

**AppImage (isteğe bağlı).** Aşama 1, Aşama 2 ve GUI için tek bir Python 3.14
çalışma zamanının paketlendiği tek dosyalık AppImage — sanal ortam yok,
sistemde `python3.11` gerekmez. Her etiketli sürüm,
[GitHub sürümler sayfasında](https://github.com/Krateian/Sutura/releases)
önceden derlenmiş `Sutura-x86_64.AppImage` ile birlikte gelir; dilerseniz
`scripts/build_appimage.sh` ile kendiniz de derleyebilirsiniz
(`dist/Sutura-x86_64.AppImage` üretir). Çalıştırılabilir yapıp çalıştırın:

```sh
chmod +x Sutura-x86_64.AppImage
./Sutura-x86_64.AppImage            # GUI
./Sutura-x86_64.AppImage model.stl  # CLI (model_fixed.stl yazar)
```

`install.sh` akışından farklı olarak AppImage sürümü kendini yerinde
güncelleyemez; yeni AppImage'ı yukarıdaki sürümler sayfasından edinin. Dolphin
sağ tık servis menüsü hâlâ `install.sh` ile kurulur.

Kurulum izole sanal ortamların içinde pip kullanır — AUR yok, yay/paru gerekmez,
sistem paket yöneticinize hiçbir şey dokunmaz. GUI, PySide6 (~79 MB indirme,
`venv`'in parçası) gerektirir; ana sanal ortamın toplam kurulu boyutu yaklaşık
800 MB'dır (isteğe bağlı fTetWild eki kendi `venv311`'ini ekler).

**Rust eklentisi (`sutura_geom`).** `sutura_geom` Rust eklentisi (~1 MB),
`install.sh` ve `install-macos.sh` tarafından otomatik olarak kurulur (GitHub
sürümünden eşleşen doğrulanmış hazır tekerlek indirilir veya Rust araç zinciri
varsa `rust/sutura-geom` üzerinden derlenir; AppImage ve macOS .app/.dmg
paketlerine dahildir). Yöntem #13 Graft (morfoloji kabuk sarma), Flap yüzey-delik
doldurma (`flap_fill`), Dressing kaplamasını (`dressing_coat`) ve Tamponad iç
tıkacını (`tamponade`) hızlandırır ve kesin dolaylı predikatları taşır.
**Eklenti zorunludur:** ne hazır tekerlek ne de kaynaktan derleme mümkünse
kurulum, algılanan platformu ve çözümü gösteren adlandırılmış bir mesajla durur
(<https://rustup.rs> adresinden Rust kurun veya `SUTURA_GEOM_WHEEL=<yol>`
ayarlayın); başlangıç ve `sutura --self-check` de sessizce geri düşmek yerine
eksik eklentiyi adıyla bildirerek hata verir. Kaynaktan derlemeye zorlamak için
`SUTURA_GEOM_FROM_SOURCE=1` kullanılabilir. Kaynaktan derleme Rust >= 1.89
(`kiddo` 6.3 MSRV) gerektirir. Eski `SUTURA_NO_GEOM=1` çıkış seçeneği
kaldırılmıştır.

**İsteğe bağlı ek — fTetWild yedek katmanı.** fTetWild yedek katmanı
`pytetwild` + `pyvista` ister ve pyvista VTK'yı da çeker: kurulu halde
yaklaşık **1,1 GB** (yalnızca VTK ~520 MB). Bu yüzden varsayılan olarak
kurulmaz (AppImage / .dmg'de de yoktur). Kurulduğunda katman, stage 1'in
kapatamadığı mesh'ler için varsayılan olarak açıktır (yukarıdaki *İsteğe
bağlı katmanlar* bölümüne bakın). Kurmak için:

```sh
SUTURA_WITH_FTETWILD=1 ./install.sh
```

Kurulmadığında varsayılan hiçbir şeyi değiştirmez (GUI kutusunun da etkisi
olmaz); yalnızca `--experimental-fallback-ftetwild` açık bir "atlandı"
raporu verir.

Ek, kurulumdan sonra installer yeniden çalıştırılmadan da yönetilebilir:
`sutura ftetwild install` / `sutura ftetwild uninstall` (`--dry-run`,
`--yes` ile) veya GUI'nin Seçenekler → **Motorlar** sekmesi, eki yalnızca
Sutura ortamına kurar/kaldırır ve önce indirme ve disk boyutlarını gösterir.
Her ikisi de desteklenmeyen düzenleri (AppImage, `.dmg`, sistem Python'u)
gerekçeyle reddeder.

Arch'ta sistemde `python3.11` yoksa `install.sh` bağımsız bir Python 3.11 çalışma zamanını yalnızca isteğe bağlı fTetWild eki (`SUTURA_WITH_FTETWILD=1`) için otomatik olarak indirir (`SUTURA_NO_PYTHON_DOWNLOAD=1` ile devre dışı bırakılabilir).

### macOS

```sh
git clone https://github.com/Krateian/Sutura.git
cd Sutura
./install-macos.sh
```

`install-macos.sh` Homebrew'i kontrol eder, conda yoksa Homebrew üzerinden
Miniforge kurar, `sutura-env` conda ortamını oluşturur (Python 3.11),
PySide6/sutura_geom'ı pip'ten kurar
(Python 3.11 ortamında PySide6 6.11.2; Linux/Python 3.12+ kurulumları 6.12.0 kullanır ve bu sabitleme macOS 13 desteğini korur),
import'ları doğrular, uygulama dosyalarını `~/.local/share/sutura/`'ya kopyalar
ve `~/.local/bin/sutura` (CLI) ile `~/.local/bin/sutura-gui` başlatıcılarını
oluşturur. Ayrıca yerel bir **`~/Applications/Sutura.app`** oluşturur — GUI'yi
doğrudan **Spotlight**'tan açabilirsiniz (`Cmd+Space`, *Sutura* yazın, Enter)
— terminal gerekmez. Yalnızca macOS içindir ve yeniden çalıştırılabilir.
İsteğe bağlı fTetWild yedek eki (~1,1 GB, Linux bölümüne bakın)
`SUTURA_WITH_FTETWILD=1 ./install-macos.sh` ile kurulur.

Kurulumcu ayrıca Finder'a bir **Quick Action** ("Sutura — Repair") ekler:
Finder'da bir veya daha fazla STL/3MF dosyası seçin, sağ tık → *Quick Actions*
→ *Sutura — Repair*. Her dosya paketlenmiş CLI ile onarılır ve sonuç yerel bir
macOS bildirimiyle raporlanır — tek dosyada `Health: X/100  Risk: Y/100
Status: <label>`, çoklu dosyada tek özet (`N/M onarıldı, K başarısız — log: ...`).
Her çalıştırmanın log'u `~/Library/Logs/Sutura/sutura-<timestamp>.log`'a
yazılır. STL/3MF olmayan dosyalar atlanır ve bildirilir. Quick Action,
`/Applications` veya `~/Applications` içinde bir PyInstaller `Sutura.app`
arar; indirilmiş (karantinalı) bir kopyaysa bildirim, önce `Sutura.app`'e sağ
tık → Aç demenizi söyler. Quick Action paketlenmiş `sutura-cli`'yi çağırdığı
için bu conda ortamının kurulu olması gerekmez.

**macOS kurulumunu kaldırma (elle):** macOS için ayrı bir kaldırma betiği
yoktur — Linux `uninstall.sh` yalnızca KDE/Linux artıklarını kapsar. macOS
kurulumunu elle kaldırmak için şunları silin:
- `~/Applications/Sutura.app` (Spotlight sarmalayıcısı) ve/veya `/Applications`
  içindeki `.app`
- `~/.local/share/sutura/` (uygulama dosyaları, `macos-quick-action.sh` dahil)
- `~/.local/bin/sutura` ve `~/.local/bin/sutura-gui`
- `~/Library/Services/Sutura Quick Action.workflow` (Finder Quick Action'ı)
- `~/Library/Logs/Sutura/` (onarım logları)
- isteğe bağlı: `sutura-env` conda ortamı (`conda env remove -n sutura-env`)

Not: conda etkileşimsiz başlatılabilir; betik `conda init` için terminali
yeniden başlatmanızı isterse öyle yapın ve betiği yeniden çalıştırın.

Her etiketli sürümde ayrıca bir **macOS .dmg** de yayınlanır
(`Sutura-vX.Y.Z.dmg`, `Build macOS .app/.dmg` workflow'uyla derlenir) —
kendi kendine yeten bir `Sutura.app` içerir. `/Applications`'a sürükleyin
(veya çift tıklayın) ve aynı şekilde Spotlight'tan açın. .dmg **imzasızdır**
(henüz Apple Developer Program / notarization uygulanmadı), bu yüzden ilk
açılışta macOS *"unidentified developer"* uyarısı gösterir. **Sağ tık → Aç**
ile açın veya karantina özniteliğini önceden temizleyin:

```sh
xattr -dr com.apple.quarantine Sutura.app
```
