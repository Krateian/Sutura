# Sutura

<p align="center">
  <img src="assets/icon/sutura-128.png" alt="Sutura" width="128">
</p>

<p align="center">
  <img src="https://github.com/Krateian/Sutura/actions/workflows/ci.yml/badge.svg" alt="CI">
</p>

STL ve 3MF dosyaları için iki aşamalı mesh onarımı; Linux için geliştirilmiş,
macOS için tam destekli.

Linux'ta Windows'un sağ tık "Fix model" (3D Builder, Netfabb) veya Bambu
Studio'nun Linux'ta bozuk olan "Fix model" düğmesinin doğrudan bir karşılığı
yoktur. Sutura bu akışı sağlar: bir mesh seç, onar, orijinali olduğu gibi
bırak.

Sutura gerçek dünya girdilerine karşı sürekli sağlamlaştırılır — Thingi10K
modelleri, bozuk dosyalar, düşmanca girdiler ve işkence senaryoları (dev
meshler, ince duvarlar, çok parçalı montajlar) — ve her değişiklik her push ve
pull request'te CI tarafından otomatik doğrulanır.

## Ekran Görüntüsü

![Sutura GUI](assets/screenshot.png)

## Neden iki aşama

* **Aşama 1 - PyMeshLab (VCG).** Yineleyen ve dejenere yüzleri kaldırır,
  non-manifold kenar ve köşeleri onarır, yüzleri tutarlı yönlendirir, *her*
  boyuttaki deliği kapatır ve küçük açık döküntü bileşenlerini atar. VCG,
  3D-baskı onarımı için kanıtlanmış klasiktir.
* **Aşama 2 - manifold3d.** Kapalı meshi geçerli bir manifold katı olarak
  yeniden kurar ve örtüşen kabukları boole birleşimiyle birleştirir. Bu, Bambu
  Studio'nun da kullandığı kütüphanedir; çıktının tek kapalı iki-manifold
  olmasını garanti eder.

Her aşama diğerinin yapamadığını düzeltir: VCG büyük delikleri kapatır ama
kendisiyle kesişen geometriyi çözmez; manifold3d su geçirmez bir sonuç garanti
eder ama Python bağlaması zaten kapalı olmayan hiçbir girdiyi reddeder
(`Error.NotManifold`), bu yüzden aşama 1 önce meshi bitirmelidir.

Linux'ta aşama 2, ayrı bir python3.11 sanal ortamında çalışır (manifold3d
Python 3.14 için wheel sunmaz). Tek ortamlı kurulumlarda (macOS/conda veya
manifold3d'ün mevcut Python'dan içe aktarılabildiği her kurulum) aşama 2
yerinde (in-process) çalışır. manifold3d hiç yoksa, rapor açıkça `Stage 2
skipped: manifold3d not available in this environment.` der — asla sessizce
atlanmaz.

Orijinal dosya asla üzerine yazılmaz. Çıktı aynı dizinde `_fixed` sonekiyle
yazılır.

## Gereksinimler

Linux:

* `python3` (>= 3.11) venv desteğiyle, PyMeshLab venv'i için
* özellikle `python3.11`, manifold3d venv'i için (manifold3d yalnızca 3.13'e
  kadar wheel sunar)
* Dolphin servis menüsü için KDE Plasma (isteğe bağlı; CLI ve GUI her yerde
  çalışır)

macOS (Apple Silicon / Intel):

* Homebrew ve Miniforge (conda). pymeshlab'ın Apple Silicon için PyPI wheel'i
  yoktur, bu yüzden conda-forge'dan gelmelidir; macOS bu yüzden Linux'un
  yalnızca pip akışı yerine tek bir conda ortamı kullanır (`install-macos.sh`).
* Python 3.11'i conda ile kurun (`install-macos.sh` bunu otomatik yapar).

Linux Python 3.11 kurulumu:

* Arch / CachyOS: `sudo pacman -S python311`
* Debian / Ubuntu 22.04+: `sudo apt install python3.11 python3.11-venv`
* Fedora: `sudo dnf install python3.11`

Diğer dağıtımlarda, varsayılan `python3` *3.11 ise* ek kurulum gerekmez.

## Sorun Giderme

* **`python3.11` bulunamadı.** Aşama 2 (manifold3d) Python 3.11 gerektirir
  çünkü yalnızca 3.13'e kadar wheel sunar. Dağıtıma göre kurun:
  * Arch / CachyOS: `sudo pacman -S python311`
  * Debian / Ubuntu 22.04+: `sudo apt install python3.11 python3.11-venv`
  * Fedora: `sudo dnf install python3.11`
  Sonra `install.sh`'ı tekrar çalıştırın — mevcut sanal ortamları yeniden
  kullanır.
* **PySide6 kurulumu başarısız.** GUI, PyPI'dan `venv` içine kurulan
  `PySide6-Essentials` gerektirir. `pip install PySide6-Essentials`'ın
  başarısız olduğu dağıtımlarda (eksik derleme araçları veya engellenen PyPI),
  sistem Qt Python bağlamalarını kurun ve giriş noktasını onlara yönlendirin:
  * Debian/Ubuntu: `sudo apt install python3-pyside6`
  * Fedora: `sudo dnf install python3-pyside6`
  * Arch: `sudo pacman -S pyside6` (resmi `extra` deposunda)
* **GUI'nin KDE dosya diyaloğu yok.** Yerel diyalog `plasma-integration` ve
  PySide6'nın Qt'siyle eşleşen bir sistem Qt sürümü gerektirir. Lastik-bant
  seçim yoksa Qt gömülü diyaloğuna geri döner — çoklu seçim için Ctrl/Shift+tık
  yine de çalışır.

## Kurulum

### Linux

Tek satır (en güncel `main`'i çeker ve kurar):

```sh
curl -fsSL https://raw.githubusercontent.com/Krateian/Sutura/main/install.sh | bash
```

Veya bir klondan:

```sh
git clone https://github.com/Krateian/Sutura.git
cd Sutura
./install.sh
```

Bu, `~/.local/share/sutura` altında iki sanal ortam oluşturur, CLI sarmalayıcıyı
`~/.local/bin/sutura`'ya kurar, hicolor uygulama ikonlarını kurar ve Dolphin
servis menüsünü kaydeder. Yeniden çalıştırmak güvenlidir.

Kurulum izole sanal ortamların içinde pip kullanır — AUR yok, yay/paru gerekmez,
sistem paket yöneticinize hiçbir şey dokunmaz. GUI PySide6 (~79 MB indirme,
`venv`'in parçası) gerektirir; iki sanal ortamın toplam kurulu boyutu kabaca
800 MB'tır.

Arch'ta `python311` kurulu değilse önce kurun (yukarıya bakın).

### macOS

```sh
git clone https://github.com/Krateian/Sutura.git
cd Sutura
./install-macos.sh
```

`install-macos.sh` Homebrew'ü kontrol eder, yoksa Homebrew üzerinden Miniforge
kurar, `sutura-env` conda ortamı oluşturur (Python 3.11), pymeshlab'ı
conda-forge'dan, manifold3d/trimesh/PySide6'yı pip'ten kurar, import'ları
doğrular, uygulama dosyalarını `~/.local/share/sutura/`'ya kopyalar ve
`~/.local/bin/sutura` (CLI) ile `~/.local/bin/sutura-gui` başlatıcılarını
oluşturur. Yalnızca macOS'a özgüdür ve yeniden çalıştırılabilir.

Not: conda etkileşimsiz başlatılabilir; script `conda init` için terminali
yeniden başlatmanızı isterse öyle yapın ve yeniden çalıştırın.

## Kullanım

Kurulumdan sonra tamamen çevrimdışı çalışır — telemetri yok, onarım sırasında
ağ çağrısı yok, kurulduktan sonra internetsiz çalışır.

Sutura GitHub'dan güncellemeleri yalnızca opt-in ederseniz kontrol eder
(varsayılan kapalı). Güncellemeler önceki kurulumu otomatik yedekler ve yeni
sürüm kendi kendini kontrolü geçemezse geri alır — güncelleme kontrolünün
ötesinde hiçbir veri gönderilmez.

CLI:

```sh
sutura model.stl            # model_fixed.stl yazar
sutura model.3mf -o fixed.3mf
sutura model.stl --human    # insan-okunur rapor
sutura model.stl --human --defects   # ayrıca girdi deliklerini / non-manifold bölgeleri listele
sutura a.stl b.3mf c.stl    # batch: her dosya bir _fixed çıktı alır
```

Birden çok dosyada, her girdi sırayla onarılır ve bir özet basılır (`N
su geçirmez, M uyarılı, K başarısız`), oluşan uyarı/hata türlerinin dökümüyle
(hacim değişimi, Stage 2 atlandı, kısmi onarım, bozuk girdi). Herhangi bir
dosya başarısız olursa çıkış kodu sıfır değildir. JSON modda her dosyanın
raporu ayrıca bir `category` (`watertight`/`warning`/`error`) ve `issues`
listesi taşır ve batch özeti `summary.issue_counts` kazanır. `-o` yalnızca tek
dosyayla geçerlidir. JSON modda her dosyanın raporu ayrıca girdinin deliklerini
(merkez, çap) ve non-manifold bölgelerini anlatan bir `defects` listesi içerir;
`--human` modda bu liste yalnızca `--defects` verildiğinde gösterilir, böylece
varsayılan rapor kısa kalır. Çap değerleri, yaygın STL/3MF kuralı olarak
milimetre varsayar; dosyanız farklı bir birim kullanıyorsa yorumu buna göre
ölçekleyin.

Bir mesh yalnızca aşama 2 gerçekten çalışıp kapalı katıyı doğruladığında
**su geçirmez** sayılır. Aşama 1 bir meshi kapatır ama aşama 2 atlanır, hata
verir veya hiç çalışmazsa (ör. macOS/conda yerinde geri dönüşün kullanılamaması)
dosya su geçirmez değil, uyarı olarak raporlanır.

Çok nesneli 3MF dosyaları yerel olarak işlenir: her nesne meshi bağımsız
onarılır ve arşive geri yazılır, böylece hiçbir nesne kaybolmaz. Rapor sonucu
nesne başına listeler (kalan delik, iki-manifold). Kusurlar da nesne başına
hesaplanır (`object_reports[i].defects`); 3MF için üst düzey toplu bir
`defects` alanı yoktur. Not: batch raporunda olduğu gibi, bayt-bayt özdeş
geometriye sahip nesneler tekilleştirilir: yalnızca ilk görülen raporlanır.

GUI:

```sh
~/.local/share/sutura/gui.py
```

GUI batch onarımı destekler: istediğiniz kadar dosya ekleyin, **Onar**'a basın
ve her biri sırayla işlenir, sonucu dosya başına listelenir. Batch bittiğinde
log'un üstünde bir özet şeridi belirir (`X su geçirmez, Y uyarılı, Z
başarısız`), etkilenen dosya sayısıyla uyarı/hata türlerini listeleyen
tıklanabilir bir **sorunları göster** bağlantısıyla. Bir dosya seçmek, girdi
kusurlarını (çapıyla delikler ve non-manifold bölgeler) log'un altındaki bir
panelde, batch özet şeridinden ayrı gösterir. Dosyalar **Dosya ekle…** (yerel
çoklu seçim, lastik-bant dahil), **Klasör ekle…** (klasördeki her
`.stl`/`.3mf`, tek seviye) veya dosya/klasörleri pencereye sürükleyerek
eklenebilir. **Durdur** çalışan onarımı sonlandırır ve kalan dosyaları
iptal edilmiş olarak işaretler. Sürükle & bırak yerel Wayland oturumlarında
çalışır (GUI bir Qt uygulamasıdır, XWayland değil).

#### Kusur detay paneli

![Kusur detay paneli](assets/defect-panel.png)

Bir dosya seçildiğinde log'un altındaki panel, girdi meshinde bulunan
kusurları listeler: her deliğin merkezi ve çapı (mm) ve her non-manifold
bölge. Bu, log'un üstündeki batch özet şeridini tamamlar — şerit batch başına
bir sayımdır, bu panel dosya başına detaydır.

Dolphin: bir STL/3MF dosyasına sağ tık -> **Sutura ile Onar**. Tek seçimde GUI
dosya yüklü açılır; çoklu seçimde her dosya başsız onarılır ve bir özet
diyaloğu gösterilir.

Servis menüsünü kurduktan veya kaldırdıktan sonra `kbuildsycoca6` çalıştırın
(kurulum bunu otomatik yapar) veya Dolphin'i yeniden başlatın.

## Mesh türüne duyarlı onarım

Sutura, bir girdi meshinin **mekanik** mi (küp, dişli, CAD parçası) yoksa
**organik** mi (heykel, taranmış model) olduğunu salt geometriden — komşu
yüzlerin dihedral açıları, numpy ile hesaplanır — sezgisel olarak tahmin eder.
Bu bir ML modeli *değildir* ve bilinçli olarak muhafazakârdır: yalnızca yüksek
güvenli durumlarda harekete geçer, aksi halde `unknown` bildirir ve bu durumda
tarihsel varsayılan Aşama 1 parametreleri değiştirilmeden kullanılır.

Tespit edilen tür GUI kusur-paneli başlığında (ör. `Tespit edilen: mechanical
(0.23)`) ve `--human` raporunda bir `Type:` satırı olarak gösterilir; JSON
raporu `detected_type` ve `detected_confidence` taşır.

Sınıflandırıldığında tür iki Aşama 1 eşiğini ayarlar:

| Tür | `mincomponentsize` (döküntü eşiği) | `maxholesize` (delik dolgusu) | Etki |
|---|---|---|---|
| mekanik | 4 | 300 | küçük keskin detayları koru, aşırı büyük delik yamalarından kaçın |
| organik | 12 | 1000 | tarama döküntüsünü daha agresif at, büyük açık bölgeleri kapat |
| unknown | 8 | 1000 | tarihsel varsayılanlar (değişmez) |

> Bu tür başına değerler **deneysel başlangıç noktalarıdır**, gerçek onarım
> verisiyle kalibre edilmemiştir — ihtiyatlı, geri alınabilir bir seçimdir.
> Yalnızca yukarıdaki iki eşik kayar; daha fazla örnek toplandıkça
> `repair.py`'de ayarlanabilirler.

### Sınıflandırıcının bilinen sınırlaması

Eğrisel-ama-mekanik parçalar (ör. bir silindir, mil veya yuvarlatılmış
geometri) sınıflandırılmaz — `unknown` kovasına düşer ve varsayılan
parametreleri korur. Bu bilinçli bir ödünleşimdir: sınıflandırıcı yalnızca açık
şekilde düz/keskin mekanik veya açık şekilde pürüzsüz organik meshlerde
devreye girer ve yanlış bir parametre seti uygulamaktansa hiçbir şey yapmayı
tercih eder.

## Test

Sentetik kırık mesh:

```sh
python3 tests/make_broken_stl.py /tmp/broken.stl
sutura /tmp/broken.stl --human
```

Üretici, eksik bir yüzü, ters sarmalanmış bir yüzü, yinelenen bir yüzü, bir
yüzgeç üçgenini ve kendisiyle kesişen bir üçgeni olan bir küp üretir.

Regresyon süitleri:

```sh
python3 tests/make_layered_multiobject_3mf.py --check   # katmanlı çok nesneli 3MF
python3 tests/test_adversarial.py                       # bozuk girdi işleme
```

`tests/real-world-samples/` içindeki gerçek dünya örnekleri
[Thingi10K](https://ten-thousand-models.appspot.com/) veri kümesinden gelir
(Zhou & Jacobson): üç gerçekten kırık model — biri non-manifold, biri
kendisiyle kesişen, biri her ikisi. Thingi10K meta verisindeki orijinal
lisanslarını korurlar; ayrıntılar ve her birinden beklenen onarım sonucu için
`tests/real-world-samples/README.md`'ye bakın.

İşkence testleri zor-ama-basılabilir geometriyi kapsar:

```sh
python3 tests/torture_tests.py
```

Bu dört senaryoyu çalıştırır ve her biri için önce/sonra raporlar: bir 5M
üçgenli küre (onarım süresi), 0.05 mm ince levha (özellik kaybı riski — sağlam
kalmalıdır), çok parçalı montaj (8 yüzlü döküntü kaldırma eşiği meşru parçaları
silmemelidir) ve çok sayıda mikro çatlaklı kaba bir tarama tarzı mesh (kalan
delik beklentisi).

## Sağlamlık

Bozuk veya düşmanca girdiler, açık bir hata ve sıfır olmayan bir çıkış koduyla
reddedilir; asla çökme veya sessizce yanlış sonuç:

| Girdi | Davranış |
|---|---|
| Kesilmiş / yarıda kalmış ikili STL | reddedilir: "Unable to open file ... Malformed file" |
| Başlık, dosyanın tuttuğundan fazla üçgen iddia eder | reddedilir: "Malformed file" |
| NaN/Infinity köşe koordinatları | reddedilir: "input mesh contains NaN or infinite coordinates" |
| Boş mesh (0 üçgen) | reddedilir: "input mesh is empty (no triangles)" |
| Tamamen dejenere mesh (yalnızca sıfır alanlı yüzler) | reddedilir: "all faces are degenerate; nothing to repair" |
| Yanlış uzantı (`.stl` içinde OBJ içeriği veya tersi) | reddedilir: "Unable to open file" |

Bunların herhangi biri çıkış kodu 1 döndürür, böylece scriptler hatayı
güvenilir şekilde algılayabilir.

## Kütüphaneler

| Kütüphane | Rol | Neden |
|---|---|---|
| PyMeshLab | aşama 1 filtre zinciri | VCG tabanlı, baskı onarımı için kanıtlanmış, her boyuttaki deliği doldurur, Python 3.14 wheel'i mevcut |
| manifold3d | aşama 2 katı yeniden kurma | su geçirmezlik garantisi, sağlam boole, Bambu Studio ile aynı motor |
| trimesh | aşama 2 G/Ç | manifold venv'inde OBJ/mesh yükleme |

`requirements.txt` ve `requirements-311.txt` içinde sabitlenmiştir.

## Bilinen sınırlamalar

* **macOS'ta henüz sağ tık / Finder entegrasyonu yok.** macOS'ta yalnızca CLI
  ve GUI mevcuttur; Linux'un Dolphin servis menüsünün ("Sutura ile Onar") bir
  karşılığı yoktur. macOS kullanıcıları terminalden `sutura-gui` veya
  `sutura <dosya>` çalıştırır.
* **Yerel KDE dosya diyaloğu.** GUI, QFileDialog'un yerel KDE diyaloğunu
  (lastik-bant dikdörtgen seçimi dahil) kullanması için
  `QT_QPA_PLATFORMTHEME=kde` ayarlar ve `QT_PLUGIN_PATH`'i
  `/usr/lib/qt6/plugins`'e yönlendirir. Bu, sistem Qt sürümünün paketlenmiş
  PySide6 Qt'siyle eşleştiğini varsayar; bunun farklı olduğu diğer
  dağıtımlarda Qt gömülü diyaloğuna geri döner — dikdörtgen seçim
  kullanılamayabilir, ama Ctrl/Shift+tık her zaman çalışır.
* **Tek bağlı kabuk içinde kendisiyle kesişimler.** manifold3d meshi bir katı
  olarak yeniden kurar, bu iç/örtüşen geometriyi çözer, ama patolojik
  durumlarda yeniden kurma özellikleri hafifçe şekillendirebilir. Sonucu her
  zaman bir dilimleyicide kontrol edin.
* **Büyük delik yamaları.** VCG delikleri düz üçgen yamalarla doldurur; çok
  büyük delikler için dolgu basit bir yamadır, akıllı bir yeniden yapılandırma
  değildir. Meshi kapatır ama yama kalitesi ortalamadır ve yumuşatma
  gerektirebilir.
* **Minicik bağlantısız döküntüler.** 8 yüzden az bileşenler kaldırılır. Ana
  gövdeye bağlı olmayan küçük meşru bir parça da kaldırılır.
* **Ters çevrilmiş tüm modeller.** Onarılan hacim negatif çıkarsa tüm mesh
  çevrilir; tutarlı şekilde "içi dışında" sarmalanmış bir model otomatik
  düzeltilir.
* **manifold3d Python bağlaması.** Açık kenarlı herhangi bir girdiyi reddeder;
  aşama 1 bir deliği kapatamazsa aşama 2 atlanır ve aşama 1 sonucu olduğu gibi
  kullanılır (rapor söyler).
* **Katmanlı/yinelenen-köşeli 3MF dışa aktarımları.** Bazı dilimleyiciler
  (Bambu Studio dahil) nesnelerinin her köşe konumunu ~15x ayrı köşe girişi
  olarak tekrarlayan ve yüzeyleri katlanmış (bir kenarda birkaç yüz çakışık)
  olan 3MF'ler yazar. VCG bu tür meshleri geçerli 2-manifoldlara
  dönüştürebilir, ama `close_holes`'un doldurmayı reddettiği birkaç
  milimetre-altı çatlak kalabilir (dolgu yaması dejenere olurdu). Sonuç
  iki-manifolddur ama her zaman tam su geçirmez değildir; çoğu dilimleyici bu
  kadar küçük çatlakları içe aktarımda otomatik iyileştirir. Geliştirmeden
  örnek: 2 nesneli bir Bambu dışa aktarımı, en iyi olası VCG geçişinden sonra
  nesne başına 13 ve 26 kalan mikro delikle bitti.
* **Tüm nesneler korunur.** Çok nesneli 3MF'ler nesne nesne onarılır ve geri
  yazılır, böylece hiçbir nesne kaybolmaz. Nesne başına sonuç CLI çıktısında ve
  GUI'de raporlanır.

## Katkı

Eksik bir özellik mi var? Onarılmayan bir mesh mi buldun? Bir issue açın. İyi
bir hata raporu çıplak bir "çalışmıyor"dan çok daha değerlidir, bu yüzden bir
mesh bildirirken lütfen şunları ekleyin:

* `sutura <dosya> --human` çıktısı (veya JSON raporu),
* çalıştırdığınız komut,
* ve biliyorsanız, meshin nasıl üretildiği — dilimleyici, tarayıcı, CAD
  dışa aktarımı vb.

Bu, kök nedeni bulmayı çok kolaylaştırır. Yapılandırılmış raporlar teşvik
edilir: `.github/ISSUE_TEMPLATE/bug_report.md`'ye bakın.

## Lisans

Apache License 2.0. `LICENSE`'e bakın.

Bu proje ayrıca atıf koruyan bir `NOTICE` dosyası içerir (Apache 2.0 §4d);
Sutura'yı yeniden dağıtır veya üzerine inşa ederseniz lütfen sağlam tutun.
