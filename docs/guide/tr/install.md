# Kurulum ve gereksinimler

[← README'ye dön](../../../README.tr.md) · [English](../install.md)

## Gereksinimler

Linux:

* `python3` (>= 3.11), venv desteğiyle, onarım venv'i için. Sistem Python'u
  venv oluşturamıyorsa kurulum otomatik olarak taşınabilir bir Python 3.11
  indirir.
* `sutura_geom` doğrulanmış hazır bir wheel'den kurulur; desteklenen
  platformlarda Rust, cmake veya C++ derleyicisi gerekmez. Kaynaktan derleme
  yalnızca araç zinciri zaten varsa yedek yoldur.
* Dolphin servis menüsü için KDE Plasma (isteğe bağlı; CLI ve GUI her yerde
  çalışır)

macOS (Apple Silicon / Intel):

* Miniforge/conda. conda yoksa `install-macos.sh` aynı çalışmada Miniforge'u
  (~80 MB) indirip kurar; mevcut bir conda kurulumu yeniden kullanılır.
  Homebrew gerekmez.
* Python 3.11, `sutura-env` conda ortamında otomatik oluşturulur.

Python 3.11, fTetWild katmanı için yalnızca ana yorumlayıcı 3.10–3.14
aralığının dışındayken yedek çalışma zamanı olarak kullanılır; güncel bir
`python3` ile normal kurulum buna ihtiyaç duymaz.

* Arch: Python 3.11 resmi depolarda bulunmaz; gerektiğinde `install.sh` otomatik olarak taşınabilir bir yedek çalışma zamanı indirir (`SUTURA_NO_PYTHON_DOWNLOAD=1` ile devre dışı bırakılabilir)
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

Bu, `~/.local/share/sutura` altında ana sanal ortamı oluşturur (ana
yorumlayıcı 3.10–3.14 aralığının dışındayken ayrıca bir `venv311` yedeği),
CLI sarmalayıcıyı `~/.local/bin/sutura`'ya kurar, hicolor uygulama ikonlarını
kurar ve Dolphin servis menüsünü kaydeder. Yeniden çalıştırmak güvenlidir.
Kurulumcu `~/.local/bin` yolunu kabuğunuzun okuduğu başlangıç dosyasına ekler
(zsh için `.zshrc`, bash için `.bashrc`/`.bash_profile`) ve geçerli terminal
için tam `~/.local/bin/sutura` yolunu yazdırır.

`./uninstall.sh` kurulu dosyaları ve PATH satırını kaldırır; kullanıcı verisi
(`~/.local/share/sutura/data`), yapılandırmalar (`~/.config/sutura`) ve
`sutura-env` conda ortamı korunur. Sutura, `sutura-env`'i yalnızca kurulumcusu
oluşturduysa kaldırır: etkileşimli çalışmada tam yolu ve boyutu gösterip sorar;
etkileşimsiz çalışmada `--remove-env` (veya `--purge`) verilmedikçe korunur.
`SUTURA_UNINSTALL_SKIP_CONDA=1` her zaman korur. `--purge` (etkileşimliyse
onaylı) kullanıcı verilerini, yapılandırmaları ve Sutura'nın oluşturduğu ortamı
da kaldırır. Eski kurulumlarda işaret yoksa ortam korunur ve elle kaldırma
komutu yazdırılır.

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
800 MB'dır (fTetWild katmanı yaklaşık 6 MB ekler, aşağıya bakın).

**Rust eklentisi (`sutura_geom`).** `sutura_geom` Rust eklentisi (~1 MB),
`install.sh` ve `install-macos.sh` tarafından doğrulanmış hazır bir abi3
wheel'den otomatik kurulur. Kurulumcu sürüm varlığını kısa süre yeniden dener,
sonra en güncel uyumlu abi3 wheel'i kullanır; desteklenen platformlarda Rust
veya cmake gerekmez. Kaynaktan derleme yalnızca wheel yoksa ve araç zinciri
zaten varsa çalışır. AppImage ve macOS .app/.dmg paketlerine dahildir. Yöntem #13 Graft (morfoloji kabuk sarma), Flap yüzey-delik
doldurma (`flap_fill`), Dressing kaplamasını (`dressing_coat`) ve Tamponad iç
tıkacını (`tamponade`) hızlandırır ve kesin dolaylı predikatları taşır.
**Eklenti zorunludur:** ne hazır wheel ne de mevcut bir kaynak araç zinciri
varsa kurulum tek bir platform mesajıyla durur; başlangıç ve
`sutura --self-check` de sessizce geri düşmek yerine eksik eklentiyi adıyla
bildirerek hata verir. Yerel wheel için `SUTURA_GEOM_WHEEL=<yol>`, araç zinciri
varsa kaynaktan derlemeye zorlamak için `SUTURA_GEOM_FROM_SOURCE=1` kullanılır.
Kaynaktan derleme Rust >= 1.89 (`kiddo` 6.3 MSRV) gerektirir. Eski
`SUTURA_NO_GEOM=1` çıkış seçeneği kaldırılmıştır.

**fTetWild yedek katmanı (varsayılan olarak kurulur).** fTetWild yedek
katmanı yalnızca `pytetwild` ister — PyVista veya VTK yok — bu yüzden kurulu
halde yaklaşık **6 MB**'dır. `install.sh` bunu varsayılan olarak ana sanal
ortama kurar (AppImage ve macOS `.dmg` içinde de gelir). Kurulduğunda katman,
stage 1'in kapatamadığı mesh'ler için varsayılan olarak açıktır (yukarıdaki
*İsteğe bağlı katmanlar* bölümüne bakın). Atlamak için:

```sh
SUTURA_WITH_FTETWILD=0 ./install.sh
```

Kurulmadığında varsayılan hiçbir şeyi değiştirmez (GUI kutusunun da etkisi
olmaz); yalnızca `--experimental-fallback-ftetwild` açık bir "atlandı"
raporu verir.

Katman, kurulumdan sonra installer yeniden çalıştırılmadan da yönetilebilir:
`sutura ftetwild install` / `sutura ftetwild uninstall` (`--dry-run`,
`--yes` ile) veya GUI'nin Seçenekler → **Motorlar** sekmesi, katmanı yalnızca
Sutura ortamına kurar/kaldırır ve önce indirme ve disk boyutlarını gösterir.
Her ikisi de paketlenmiş veya sistem düzenlerini (AppImage, `.dmg`, sistem
Python'u) değiştirmeyi reddeder ve gerekçesini bildirir.

Arch'ta `install.sh`, aralık içinde bir `python3` yoksa bağımsız bir Python 3.11 çalışma zamanını yalnızca fTetWild yedek yorumlayıcısı olarak otomatik indirir (`SUTURA_NO_PYTHON_DOWNLOAD=1` ile devre dışı bırakılabilir).

### macOS

```sh
git clone https://github.com/Krateian/Sutura.git
cd Sutura
./install-macos.sh
```

`install-macos.sh` mevcut conda kurulumunu kullanır; conda yoksa Miniforge'u
(~80 MB) otomatik indirip kurar, `sutura-env` conda ortamını oluşturur
(Python 3.11),
PySide6/sutura_geom'ı pip'ten kurar
(Python 3.11 ortamında PySide6 6.11.2; Linux/Python 3.12+ kurulumları 6.12.0 kullanır ve bu sabitleme macOS 13 desteğini korur),
import'ları doğrular, uygulama dosyalarını `~/.local/share/sutura/`'ya kopyalar
ve `~/.local/bin/sutura` (CLI) ile `~/.local/bin/sutura-gui` başlatıcılarını
oluşturur. Ayrıca yerel bir **`~/Applications/Sutura.app`** oluşturur — GUI'yi
doğrudan **Spotlight**'tan açabilirsiniz (`Cmd+Space`, *Sutura* yazın, Enter)
— terminal gerekmez. Yalnızca macOS içindir ve yeniden çalıştırılabilir.
fTetWild yedek katmanı (kurulu halde ~6 MB, Linux bölümüne bakın) varsayılan
olarak kurulur; `SUTURA_WITH_FTETWILD=0 ./install-macos.sh` onu atlar.

Kurulumcu ayrıca Finder'a bir **Quick Action** ("Sutura — Repair") ekler. Her
seçili dosya için görünür; Finder'da bir veya daha fazla STL/3MF dosyası seçin,
sağ tık → *Quick Actions* → *Sutura — Repair*. Her dosya kurulu CLI ile onarılır (`.dmg` uygulaması paketli CLI'yi kullanabilir) ve sonuç
yerel bir macOS bildirimiyle raporlanır — tek dosyada `Health: X/100  Risk: Y/100
Status: <label>`, çoklu dosyada tek özet (`N/M onarıldı, K başarısız — log: ...`).
Her çalıştırmanın log'u `~/Library/Logs/Sutura/sutura-<timestamp>.log`'a
yazılır. STL/3MF olmayan dosyalar atlanır ve bildirilir. `.dmg` uygulamasında
Quick Action paketli `sutura-cli`'yi, conda kurulumunda ise doğrudan kurulu
`~/.local/bin/sutura` yolunu (veya conda ortamını) kullanır. Uygulama indirilmiş
(karantinalı) bir kopyaysa bildirim, önce `Sutura.app`'e sağ tık → Aç
demenizi söyler.

**macOS kurulumunu kaldırma:** `./uninstall.sh` artık macOS'u da kapsar.
Uygulamayı, başlatıcıları, Quick Action'ı, logları ve ikonları kaldırır.
`sutura-env` yalnızca kurulumcu oluşturduysa ve etkileşimli olarak onay
verirseniz (veya `--remove-env`/`--purge` verirseniz) kaldırılır; önce tam yol
ve boyut yazdırılır, etkileşimsiz çalışmada varsayılan olarak korunur.
Kullanıcı verisi (`~/.local/share/sutura/data`) ve yapılandırmalar
(`~/.config/sutura`) varsayılan olarak korunur; `--purge` (etkileşimliyse
onaylı) bunları ve sahip olunan ortamı da kaldırır. Eski kurulumlarda işaret
yoksa ortam korunur ve elle kaldırma komutu yazdırılır. Aynı betik
`.zshrc`/`.zprofile`/`.bashrc`/`.bash_profile` içine eklediği PATH satırını da
kaldırır.

Kurulumcu ayrıca kabuğunuzun okuduğu başlangıç dosyasına `~/.local/bin`
yolunu ekler (zsh için `.zshrc`, bash için `.bashrc`/`.bash_profile`) ve
geçerli terminal için tam `~/.local/bin/sutura` yolunu yazdırır.

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
