# Sutura

<p align="center">
  <img src="assets/icon/sutura-128.png" alt="Sutura" width="128">
</p>

<p align="center">
  <img src="https://github.com/Krateian/Sutura/actions/workflows/ci.yml/badge.svg" alt="CI">
  <img src="https://github.com/Krateian/Sutura/actions/workflows/build-appimage.yml/badge.svg" alt="AppImage build">
  <img src="https://github.com/Krateian/Sutura/actions/workflows/codeql.yml/badge.svg" alt="CodeQL">
  <img src="https://img.shields.io/github/v/release/Krateian/Sutura" alt="Son sürüm">
  <img src="https://img.shields.io/github/license/Krateian/Sutura" alt="Lisans">
  <img src="https://img.shields.io/github/downloads/Krateian/Sutura/total" alt="İndirmeler">
  <img src="https://img.shields.io/github/contributors/Krateian/Sutura" alt="Katkıda bulunanlar">
  <img src="https://img.shields.io/github/languages/top/Krateian/Sutura" alt="Ana dil">
  <img src="https://img.shields.io/github/repo-size/Krateian/Sutura" alt="Depo boyutu">
  <img src="https://img.shields.io/github/commit-activity/y/Krateian/Sutura" alt="Commit etkinliği">
</p>

STL, OBJ ve 3MF dosyaları için iki aşamalı mesh onarımı. Linux için geliştirildi,
macOS tam destekli.

Linux'ta Windows'un sağ tık "Fix model" (3D Builder, Netfabb) ya da Bambu
Studio'nun Linux'ta çalışmayan "Fix model" düğmesinin doğrudan bir karşılığı
yoktur. Sutura bu iş akışını sunar: mesh'i seçin, onarın, orijinaline
dokunulmadan onarılmış bir kopya üretilsin.

Sutura, gerçek dünya girdilerine karşı sürekli sağlamlaştırılır — Thingi10K
modelleri, bozuk dosyalar, düşmanca girdiler ve işkence senaryoları (devasa
mesh'ler, ince duvarlar, çok parçalı montajlar) — ve her değişiklik, her push
ve pull request'te CI tarafından otomatik doğrulanır.

**Bir not (maintainer'dan)**

Sutura'yı, önde gelen bir havayolu şirketinde surdurdugum gunduz
havacilik muhendisligi isimin yaninda tek basima gelistiriyorum.
Onumuzdeki donemde surumler eskisi kadar sik gelmeyebilir - proje
terk edilmis degil, sadece tempo yavaslayacak.

## Ekran Görüntüsü

![Sutura GUI](assets/screenshot.png)

## Neden iki aşama

* **Aşama 1 - PyMeshLab (VCG).** Yinelenen ve dejenere yüzleri kaldırır,
  non-manifold kenar ve köşeleri onarır, yüzleri tutarlı şekilde yönlendirir,
  *her* boyuttaki deliği kapatır ve küçük açık döküntü bileşenlerini atar. VCG,
  3D baskı onarımının kanıtlanmış klasiğidir.
* **Aşama 2 - manifold3d.** Kapalı meshi geçerli bir manifold katı olarak
  yeniden kurar ve örtüşen kabukları boolean birleşimiyle birleştirir. Bu,
  Bambu Studio'nun da kullandığı kütüphanedir; çıktının tek, kapalı bir
  iki-manifold olmasını garanti eder.

Her aşama diğerinin yapamadığını düzeltir: VCG büyük delikleri kapatır ama
kendisiyle kesişen geometriyi çözmez; manifold3d su geçirmez bir sonuç garanti
eder ama Python bağlaması, zaten kapalı olmayan hiçbir girdiyi reddeder
(`Error.NotManifold`), bu yüzden aşama 1'in meshi önce bitirmesi gerekir.

Linux'ta aşama 2, ayrı bir python3.11 sanal ortamında çalışır (manifold3d,
Python 3.14 için wheel sunmaz). Tek ortam kurulumlarında (macOS/conda veya
manifold3d'nin mevcut Python'dan içe aktarılabildiği her kurulum) aşama 2
bunun yerine süreç içinde (in-process) çalışır. manifold3d hiç yoksa rapor
bunu açıkça belirtir: `Stage 2 skipped: manifold3d not available in this
environment.` — asla sessizce atlanmaz.

Orijinal dosya asla üzerine yazılmaz. Çıktı aynı dizinde `_fixed` sonekiyle
yazılır.

## Özellik durumu

Bu bölüm, her ana alanın dürüst bir olgunluk özetidir: neyin sağlam olduğu,
bilinen sınırlamanın ne olduğu ve nerede dikkatli olunması gerektiği.
Yüzdeler bir ölçüm değil, bir değerlendirmedir; Sutura'ya nerede
güvenebileceğinizi ve çıktıyı nerede hâlâ yeniden kontrol etmeniz gerektiğini
söyler.

| Alan | Olgunluk | Ne sağlam / Nerede dikkatli |
|---|---|---|
| STL onarımı (iki aşamalı) | ~%96 | VCG + manifold3d hattı, bozuk/düşmanca/işkence girdilerine karşı CI ile sağlamlaştırılmış ve 75 modellik gerçek dünya korpusunda (0 sert hata; Aşama 1 zinciri yeniden düzenlendi ve `maxholesize` mesh-duyarlı hale getirildi, böylece büyük tarama delikleri kapanıyor) ayrıca 115 mesh'lik gerçek-dünya tarama korpusunda doğrulanmıştır (gerçek çıktı geometrisi üzerinde sıkı bir `defects.detect()` kapalı-döngü kontrolüyle yeniden ölçüldü: 0 çökme, 103/115 = ~%90 sıkı su geçirmez, her hat iddiası birebir doğrulandı — bkz. `docs/repair-benchmark-strict-watertight-2026-09.md`). %100 değil: patolojik kendisiyle-kesişimler aşama 2'nin yeniden kurmasında yeniden şekillenebilir ve tarama mesh'lerindeki son birkaç inatçı delik / ağır non-manifold yapı gerçek bir VCG sınırıdır. |
| 3MF çok nesneli | ~%92 | Her nesne bellekte bağımsız onarılır ve geri yazılır, böylece hiçbir nesne kaybolmaz. Aşama 1'in kapattığı her nesne artık tek-mesh dosyalarıyla aynı paylaşılan yardımcı aracılığıyla **nesne başına aşama 2** (manifold3d su geçirmez yeniden kurma) alır: nesne başına `stage2` raporları, `objects_watertight` / `objects_stage2_ok` özetleri ve dosya düzeyi karar TÜM nesneleri dikkate alır (yalnızca nesne 0'ı değil). Bayt bayt özdeş nesneler tek onarımı paylaşır ama her biri yine kendi raporunu alır. Katmanlı/yinelenen köşeli (Bambu tarzı) bir 3MF, Aşama 1'de düzeltilir (köşe tekilleştirmesinden sonra ikinci bir yinelenen-yüz geçişi) ve nesne başına 12 yüz / 0 deliğe onarılır, nesne başına aşama 2 ile su geçirmez doğrulanır. Regression testiyle doğrulanır (`tests/test_stage2_3mf.py`). Bilinen sınırlar: nesne başına aşama 2 yalnızca Aşama 1'in gerçekten kapattığı nesnelere uygulanır (açık nesneler aşama 1 çıktısı olarak kalır), nesne 0'ın `stage1`/`stage2` üst düzey alanları geriye dönük uyumluluk için korunur ve `<vertex>` ayrıştırıcısı x,y,z öznitelik sırasını varsayar. |
| Kusur tespiti (delik / non-manifold) | ~%90 | Stdlib+numpy, tek doğruluk kaynağı, temiz ve kırık küplerde birim testlerle doğrulanır. %100 değil: yalnızca girdi kusurlarını bildirir; binlerce mikro çatlaklı bir mesh'te kusur başına liste büyür ve CLI JSON'u, yalnızca çizim amaçlı dizin verisini içermez. |
| GUI | ~%89 | Yerel Qt batch onarımı, sürükle & bırak, kusur paneli, mod önerili onarım öncesi analiz, ısı haritası, öncesi/sonrası karşılaştırma (statik + yüzey sapması olan interaktif 3D görüntüleyici), **renk-kodlu kusur görünümü** (kırmızı = non-manifold, turuncu = ters sarmalım, sarı = dejenere yüz — FAZ11), **"ne değişti" onarım günlüğü paneli** (kapatılan delikler, düzeltilen non-manifold kenarlar, silinen yüzler, bileşenler, aşama 2 — FAZ11), **opt-in deneysel kutular** (edge-tiebreak sınıflandırıcı kafası; küçük-parçaları-birleştir — FAZ14), onarım modu seçici + onarım profili açılır listesi, durum/sürüm satırı, i18n (EN/TR). Eksikler: CLI'yı ayrı bir süreç olarak çağırır (süreç içi ilerleme yok), yerel KDE dosya diyaloğu yalnızca sistem Qt'si PySide6'nınkiyle eşleştiğinde çalışır ve macOS'ta Finder sağ tık onarımı GUI'nin kendisi yerine ayrı Quick Action ile sağlanır. |
| CLI | ~%90 | Sabit bayraklar (`-o`, `--human`, `--defects`, `--diff`, `--mode`, `--profile`, `--dry-run`, `--version`), salt-okunur `validate` alt komutu, JSON raporları, batch özeti, çıkış kodları. Ayrıca deneysel/prototip bayraklar: `--experimental-join-components` (küçük bileşenleri silmek yerine en yakın büyük bileşene taşır; geometriyi değiştirir, yalnızca değerlendirme) ve `--experimental-edge-tiebreak` (opt-in 11-özellikli sınıflandırıcı kafası — temel + FAZ10 taramasının beş güçlü sinyali; kazanç küçük, 71 mesh'lik etiketli sette 1 mesh, ama sinyal istatistiksel olarak gerçek; varsayılan değil). `--human` raporu yalnızca İngilizcedir (yerelleştirme yalnızca GUI'yi ilgilendirir). |
| Batch işleme | ~%90 | Dosya başına sonuçları ve bir özeti olan çok dosyalı onarım. Sert durdurma (Ctrl-C / Durdur) desteklenir; batch kaldığı yerden sürdürülemez ve başarısız bir dosya diğerlerini durdurmaz. |
| Kusur ısı haritası | ~%80 | İsteğe bağlı CPU rasterizer (GL yok), alt süreçte çalışır, GUI'yi asla çökertmez. Bilinçli olarak yalnızca CPU: ekransız sistemlerde ekran dışı OpenGL çağrıları segfault verir, bu yüzden tam GL gölgeleme yerine üç noktalı ışık modeliyle düz gölgelenir ve çok nesneli 3MF'de yalnızca ilk nesne çizilir. |
| Öncesi/sonrası karşılaştırma | ~%80 | Orijinal ve onarılmış görünümler arasında statik, CPU ile çizilmiş görüntülerin tıkla-geçişi; **en yoğun bozukluk bölgesi yakın çekimi** ve üç durumlu renk şemasıyla (gri = hiç bozulmamış, yeşil `(46,204,113)` = düzelen, turuncu `(255,140,60)` = hâlâ bozuk). Düzelen harita uzaysaldır (onarılmış yüz merkezleri, orijinal kusur uzantılarına göre) ve tarama mesh'lerinin hızlı kalması için en büyük 256 kusurla sınırlıdır. **Statik/İnteraktif** anahtarı, CPU tabanlı etkileşimli 3D görünüm ekler (sürükleyerek döndür, tekerlekle yakınlaştır; sürüklemede LOD, sonra tam çözünürlüklü son kare) ve **yüzey-sapması** modu (pymeshlab'ın en-yakın-yüzey-noktası filter'ıyla yüzey başına onarılmış→orijinal uzaklık + global Hausdorff maks.), ikisi de ilk kullanımda tembel üretilir ve diyalog başına önbelleğe alınır; interaktif LOD hedefi 75 modellik korpusa göre ayarlanır (720×540'ta medyan ~71 FPS). Isı haritasıyla aynı GL kısıtı CPU çizicisinde kalmasını gerektirir; çok nesneli 3MF'de yalnızca ilk nesne karşılaştırılır. Regression testleriyle doğrulanır (`tests/test_healed_mask.py`, `tests/test_before_after_render.py`, `tests/test_viewer_data.py`, `scripts/verify_before_after_dialog.py`). |
| Validate (`sutura validate`) | ~%55 (beta) | 0.1.8-beta.1'de yeni: delik / non-manifold bölgeler / self-intersection / bağlı bileşenler / işaretli hacim (yön) / yüzey alanı ve watertight kararının salt-okunur analizi — onarım yok, çıktı dosyası yok. Beta kalitesi: birleşik metrikler yeni ve gerçek dünya onarım sonuçlarına karşı henüz kalibre edilmemiştir; çok nesneli 3MF her nesneyi doğrular ama yalnızca asgari bir özet rapor tutar. Kendine özgü bir regression süiti vardır (`tests/test_validate.py`). |
| Dry-run (`--dry-run`) | ~%50 (beta) | 0.1.8-beta.1'de yeni: yapılacak planı bildirir (tespit edilen tür, mod, Aşama 1 eşikleri, bulunan delik / döküntü / self-intersection, aşama 2 uygunluğu) ve hiçbir şey yazmaz. Beta kalitesi: plan girdi analizinden türetilir, bu yüzden tam delik kapatma sayıları gerçek bir çalışmayla birebir uyuşacağının garantisi değildir ve extreme modun ek geçişleri simüle edilmez. `tests/test_validate.py` tarafından kapsanır (validate ve dry-run aynı süiti paylaşır). |
| Onarım modları (`--mode` merdiveni) | ~%80 | Aşama 1 eşikleri için beş kademeli agresiflik merdiveni (`low`/`medium`/`auto`/`aggressive`/`extreme`); hem CLI bayrağı hem batch geneli GUI seçici olarak sunulur; `auto`, tarihsel sınıflandırıcı + güven eşiği davranışını birebir korur ve regression testiyle doğrulanır (`tests/test_repair_mode.py`). Mod, taban `maxholesize`'ı belirler ve bu değer daha sonra mesh-duyarlı şekilde yukarı çekilir (`max(taban, 2 × en uzun girdi loop'u)`, asla düşürülmez) böylece büyük tarama delikleri her modda kapanır. Uyarılar: `extreme` küçük bir nesneyi silebilir (bu, bozuk girdi değil, ayrı `extreme_removed_object` hatası olarak raporlanır) ve tür başına ayarlı eşik değerleri deneyseldir. |
| Onarım profilleri (`--profile`) | ~%70 (yeni) | Beş adlandırılmış Aşama 1 eşik preseti (`mechanical`/`organic`/`scan`/`miniature`/`fast`), CLI veya batch geneli GUI açılır listesiyle devreye girer; yalnızca mod `auto` iken etkilidir (açık sabit mod kazanır). Varsayılan (profil yok) eskisiyle bayt-birebir aynıdır. Uyarı: `miniature` `mincomponentsize`'ı 1'e indirir (bilinçli bir opt-in; varsayılan yol `>= 8` tutar). |
| Mesh türüne duyarlı onarım | ~%75 | Sezgisel mekanik/organik tahmini iki Aşama 1 eşiğine ince ayar yapar; tür başına güven eşiğiyle sınırlanır (mekanik ≥ 0.75, organik ≥ 0.70) ve bir kalibrasyon harness'iyle ölçülür (`scripts/calibrate_classifier.py`). **Varsayılan** motor **experimental**'dir (`sutura/mesh_classifier_v2.py`): RANSAC düzlem segmentasyonu + eğrilik gelişebilirlik sinyali + küçük eğitilmiş başlık. 40 mesh'lik gerçek dünya korpusunda (36 etiketli) 29/36'ya karşı classic 16/36 (mekanik isabet 16/20'ye karşı 2/20 — classic tarayıcı yanlılığı genişletilmiş korpusta çok daha görünür), 71 mesh'lik etiketli sette LOO-CV 0.845'e karşı classic 0.648. Classic'in unknown okuduğu kavisli-ama-mekanik parçaları (silindir, tüp, yuvarlatma) sınıflandırır; istisna/geçersiz sonuçta classic'e sessizce geri döner ve `--classifier-engine classic` / `SUTURA_CLASSIFIER_ENGINE=classic` ile eski motor seçilebilir kalır. Bir **sınır-yakını classic-uzlaşma geri dönüşü**, başlık yazı-tura durumundayken (`\|p_mech − 0.5\|·2 < 0.15`) ve classic seçilen sınıfla güçlü biçimde hemfikirken (sınıf puanı ≥ 0.70) başlığın sınıfını korur ama classic'in güvenini miras alır, böylece güçlü bir hemfikir sinyal ince ayar eşiğinde asla kaybolmaz (bu, `artec_metal-nut.stl`'yi su geçirmezliğe geri getirdi — tarama korpusunda 103/115). Deneysel: tür başına değerler tahmini başlangıç noktalarıdır, zor serbest-form/kavisli örnekler (bükülmüş boru, sarmal boru, kavisli kanca, dekoratif kâseler mekanik okunur) hâlâ ıskalanır ve kendinden emin ama yanlış bir v2 tahmini classic'e karşı yeniden kontrol edilmez — tarama kaynaklı girdide tespit edilen türü temkinli yorumlayın. |
| Onarım güven skoru | ~%70 (beta) | Mevcut onarım sinyallerini (aşama 2 sonucu, kalan delikler, sınıflandırıcı güveni, eşik ayarı, onarım modu, self-intersection'lar, hacim değişimi) tek bir 0–100 skorda Yüksek/Orta/Düşük etiketiyle birleştirir: onarılan dosyalarda `repair_confidence`, validate / --dry-run'da `estimated_confidence` ("sonuç farklı olabilir" uyarısıyla). Regression testiyle doğrulanır (`tests/test_confidence.py`). Deneysel: ağırlıklandırma modeli yeni ve gerçek kullanıcı geri bildirimiyle henüz doğrulanmadı. |
| Onarım Sağlığı / Riski | ~%60 (yeni) | Klasik güvenin üzerine ayrı, eklemeli bir skorlama sistemi: `repair_health` (0–100, final mesh sağlamlığı: su geçirmez + non-manifold/self-intersection/delik yok) ve `repair_risk` (0–100, onarımın mesh'i ne kadar değiştirdiği: yüz/vertex/bileşen/hacim deltaları), ayrıca bir config lookup tablosundan türetilen iki-eksenli status etiketi (`safe`/`review`/`caution`/`failed`/`unavailable`). Ağırlıklar/eşikler kodda değil `sutura/repair_score_config.json`'da; fail-silent (onarımı asla bozmaz); regression testiyle doğrulanır (`tests/test_repair_score.py`). Deneysel: ağırlık değerleri ve tier sınırları başlangıç noktalarıdır. |
| Onarım bütçesi (`--max-geometry-change` / `--max-risk` / `--force`) | ~%60 (yeni) | İsteğe bağlı güvenlik sınırları: gerçek geometri değişimi (max \|hacim\|/\|yüzey\| değişim %) veya `repair_risk` skoru bütçeyi aşarsa çıktı asla sessizce kaydedilmez — TTY'de etkileşimli `[y/N]` istemi, aksi halde kayıt açık `"status": "budget_declined"` işaretiyle (sorun `budget_exceeded`, çıkış 1) reddedilir ve `--force` sormadan kaydeder. Bütçe içindeyken sayılar yine bildirilir (`budget` bloğu). Mevcut hacim/yüzey/risk metriklerini yeniden kullanır (yeniden hesaplamaz); GUI aynı bütçeleri sunar ve onay sonrası reddedilen dosyaları `--force` ile yeniden çalıştırır. Regression testiyle doğrulanır (`tests/test_budget.py`). Deneysel: 0 = sınır yok ve reddedilen kayıt, dosyanın hiç yazılmaması anlamına gelir. |
| Birim algılama uyarısı | ~%60 (yeni) | Milimetre yerine inç/santimetre cinsinden modellenmiş olabilecek modelleri işaretleyen, engelleyici olmayan bir bounding-box sezgisel kuralı (`unit_warning` + `unit_hint` JSON'da, `--human`'da bir `WARNING:` satırı, GUI log / analiz paneli / kusur panelinde gösterilir). 3MF `<model unit="...">` bildirimleri dikkate alınır: bildirilen inç/santimetre birimi aynen raporlanır, bildirilen milimetre birimi (spec varsayılanı) güvenilir ve sezgisel kural bastırılır. Onarımı asla değiştirmez — yalnızca bilgilendirmedir. Regression testiyle doğrulanır (`tests/test_units.py`). Deneysel: 25–400 mm "makul yazdırılabilir parça" aralığı ve inç-önce-santimetre sıralaması sezgiseldir, kalibrasyon değildir. |
| Çapraz platform (Linux/macOS) | ~%80 | Hem Linux (install.sh + AppImage) hem macOS (conda) çalışır, CI ikisini de kapsar; her sürümde ayrıca imzasız bir macOS `.dmg` de yayınlanır (`Build macOS .app/.dmg` workflow'u) ve macOS kurulumları yerel bir `~/Applications/Sutura.app` alır — GUI Spotlight'tan açılır (Cmd+Space → "Sutura"), ayrıca Finder'da bir **Quick Action** (`~/Library/Services/Sutura Quick Action.workflow`) sağ tıkla onarım için. Eksikler: AppImage/GUI kendini yerinde güncelleyemez (squashfs salt okunurdur), .dmg notarize edilmemiştir (Gatekeeper "unidentified developer" uyarısı gösterir) ve macOS için ayrı bir kaldırma betiği yoktur (aşağıdaki "macOS kurulumunu kaldırma" bölümüne bakın). |
| Otomatik güncelleme | ~%75 | Opt-in'dir; kendi kendini kontrol başarısız olursa yedeği alır ve geri döner. Sürüm kontrolü ön sürüm (prerelease) etiketlerini anlar, böylece beta kullanıcılara stabil sürüm çıktığında sunulur. Otomatik güncelleme v0.2.0 lisans sınırında durur: v0.1.x kurulumlar o sınırın ötesine asla sessizce yükseltilmez (yeni şartlar önce gösterilir, sürüm releases sayfasından elle kurulmalıdır). Uyarılar: yalnızca Linux/pip kurulumuna yöneliktir (AppImage yeni bir sürüm indirir) ve GitHub ile iletişim kurduğu için çevrimdışı değildir. |
| Dolphin entegrasyonu | ~%85 | STL/OBJ/3MF için sağ tık servis menüsü; tekli/çoklu seçimi destekler. KDE Plasma'ya ve `kbuildsycoca6` yenilenmesine bağlıdır; diğer dosya yöneticilerinde veya macOS'ta bulunmaz. |
| OrcaSlicer eklentisi | ~%35 — deneysel | Tek başına çalışan betik eklentisi, ama **gerçek bir OrcaSlicer'da test edilmemiştir**: yalnızca çalıştırmadığımız nightly/2.4.2+ sürümlerinde bulunan bir eklenti sistemini hedefler, `execute()` seçili modeli okuyamaz (yapılandırılmış bir dosyayı onarır) ve yalnızca Linux içindir. Bitmiş bir özellik değil, bir başlangıç noktası olarak ele alın. |
| Test kapsamı | ~%89 | Düz betik süitleri (smoke, katmanlı 3MF, düşmanca, sınıflandırma, güven, kusurlar, ısı haritası çerçeveleri, düzelen-harita, öncesi/sonrası çizimi, viewer verisi, validate/dry-run, mesh sınıflandırıcı, onarım modu, öneriler, güncelleyici, obj onarımı, birim, bütçe, stage2-3mf, işkence) her push/PR'da CI'de çalışır. %100 değil: GUI'nin otomatik bir UI testi yoktur ve canlı bir OrcaSlicer'a karşı yeniden üretilebilir uçtan uca test yoktur. |

## Gereksinimler

Linux:

* `python3` (>= 3.11), venv desteğiyle, PyMeshLab venv'i için
* ayrıca `python3.11`, manifold3d venv'i için (manifold3d yalnızca 3.13'e
  kadar wheel sunar)
* Dolphin servis menüsü için KDE Plasma (isteğe bağlı; CLI ve GUI her yerde
  çalışır)

macOS (Apple Silicon / Intel):

* Homebrew ve Miniforge (conda). pymeshlab'ın Apple Silicon için PyPI wheel'i
  yoktur, bu yüzden conda-forge'dan gelmelidir; macOS bu yüzden Linux'un
  yalnızca pip kullanan akışı yerine tek bir conda ortamı kullanır
  (`install-macos.sh`).
* Python 3.11'i conda ile kurun (`install-macos.sh` bunu otomatik yapar).

Linux Python 3.11 kurulumu:

* Arch / CachyOS: `sudo pacman -S python311`
* Debian / Ubuntu 22.04+: `sudo apt install python3.11 python3.11-venv`
* Fedora: `sudo dnf install python3.11`

Diğer dağıtımlarda, varsayılan `python3` *3.11 ise* ek kurulum gerekmez.

## Sorun Giderme

* **`python3.11` bulunamadı.** Aşama 2 (manifold3d) Python 3.11 gerektirir
  çünkü manifold3d yalnızca 3.13'e kadar wheel sunar. Dağıtıma göre kurun:
  * Arch / CachyOS: `sudo pacman -S python311`
  * Debian / Ubuntu 22.04+: `sudo apt install python3.11 python3.11-venv`
  * Fedora: `sudo dnf install python3.11`
  Sonra `install.sh`'ı yeniden çalıştırın — mevcut sanal ortamları yeniden
  kullanır.
* **PySide6 kurulumu başarısız.** GUI, `venv`'e PyPI'dan kurulan
  `PySide6-Essentials` gerektirir. `pip install PySide6-Essentials`'ın
  başarısız olduğu dağıtımlarda (eksik derleme araçları veya engellenen PyPI),
  sistem Qt Python bağlamalarını kurun ve giriş noktasını onlara yönlendirin:
  * Debian/Ubuntu: `sudo apt install python3-pyside6`
  * Fedora: `sudo dnf install python3-pyside6`
  * Arch: `sudo pacman -S pyside6` (resmi `extra` deposunda)
* **GUI'de KDE dosya diyaloğu yok.** Yerel diyalog, `plasma-integration` ve
  PySide6'nın Qt'siyle eşleşen bir sistem Qt sürümü gerektirir. Lastik bant
  seçimi yoksa Qt, gömülü diyaloğuna geri döner — çoklu seçim için Ctrl/Shift+tık
  yine de çalışır.
* **macOS: `pip install pymeshlab` başarısız veya Intel derlemesi çekiyor.**
  PyMeshLab'ın Apple Silicon PyPI wheel'i yok — conda-forge'dan kurun
  (`conda install -n sutura-env -c conda-forge pymeshlab`, bkz.
  `install-macos.sh`). Kurulum, import sırasında yüklenen yerel VCG
  eklentilerinden oluşan bir dizindir (`pmeshlab.*.so`, `PlugIns/*.so`,
  `lib/*.so`, `Frameworks/*.dylib`); bir filter "not loaded" derse paketin
  yanında `PlugIns/` eksiktir. PyInstaller ile paketlerken yalnızca hidden
  import değil `--collect-data pymeshlab` gerekir.
* **Headless GUI "açılışta takılıyor".** İlk çalıştırmada (config yok,
  `~/.config/sutura/config.json`) **modal** güncelleme/kullanım-geçmişi
  diyaloğu açılır ve yanıtlanana kadar engeller. Headless çalıştırmak için
  config'i önceden oluşturun, ör. `{"check_for_updates": false,
  "history_enabled": true}`. Güncelleme sorusu AppImage build'lerinde
  atlanır; geçmiş sorusu ilk çalıştırmada her zaman sorulur.
* **macOS: her `git push`'ta `grep: empty (sub)expression`.** Pre-push
  güvenlik hook'unun secret deseni, BSD grep'in reddettiği boş bir alternation
  dalı içeriyordu; secret taraması sessizce hiç çalışmıyordu.
  `scripts/pre-push-security-check.sh`'te düzeltildi (opsiyonel grup) — hook'u
  `./install.sh` / `./install-macos.sh` ile yeniden kurun.
* **Standalone .app: stage 2 atlandı / düşük güven.** Aşama 2 (manifold3d)
  bundled CLI'nin yanında `manifold_bridge.py` ister (ör.
  `Sutura.app/Contents/MacOS/sutura-cli/manifold_bridge.py`) ve
  `manifold3d`/`trimesh` bundle içinde olmalı; aksi halde
  `stage2_bridge_available=false` olur ve güven ~25 puan düşer.

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

Bu, `~/.local/share/sutura` altında iki sanal ortam oluşturur, CLI sarmalayıcıyı
`~/.local/bin/sutura`'ya kurar, hicolor uygulama ikonlarını kurar ve Dolphin
servis menüsünü kaydeder. Yeniden çalıştırmak güvenlidir.

**AppImage (isteğe bağlı).** Her iki Python çalışma zamanının da paketlendiği
tek dosyalık AppImage — sanal ortam yok, sistemde `python3.11` gerekmez
(aşama 1 + GUI için 3.14, aşama 2 için 3.11 paketlenir). Her etiketli sürüm,
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
`venv`'in parçası) gerektirir; iki sanal ortamın toplam kurulu boyutu yaklaşık
800 MB'dır.

Arch'ta `python311` kurulu değilse önce kurun (yukarıya bakın).

### macOS

```sh
git clone https://github.com/Krateian/Sutura.git
cd Sutura
./install-macos.sh
```

`install-macos.sh` Homebrew'i kontrol eder, conda yoksa Homebrew üzerinden
Miniforge kurar, `sutura-env` conda ortamını oluşturur (Python 3.11),
pymeshlab'ı conda-forge'dan, manifold3d/trimesh/PySide6'yı pip'ten kurar,
import'ları doğrular, uygulama dosyalarını `~/.local/share/sutura/`'ya kopyalar
ve `~/.local/bin/sutura` (CLI) ile `~/.local/bin/sutura-gui` başlatıcılarını
oluşturur. Ayrıca yerel bir **`~/Applications/Sutura.app`** oluşturur — GUI'yi
doğrudan **Spotlight**'tan açabilirsiniz (`Cmd+Space`, *Sutura* yazın, Enter)
— terminal gerekmez. Yalnızca macOS içindir ve yeniden çalıştırılabilir.

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

## Kullanım

Kurulumdan sonra tamamen çevrimdışı çalışır — telemetri yok, onarım sırasında
ağ çağrısı yapılmaz, kurulduktan sonra internetsiz çalışır.

Sutura, güncellemeleri GitHub'dan yalnızca opt-in yaparsanız kontrol eder
(varsayılan kapalıdır). Güncelleme önceki kurulumu otomatik olarak yedekler ve
yeni sürüm kendi kendini kontrolü geçemezse geri alır — güncelleme kontrolünün
ötesinde hiçbir veri gönderilmez. Otomatik güncelleme v0.2.0'da durur: v0.2'den
itibaren lisans PolyForm Noncommercial 1.0.0'a geçiyor (ticari kullanım için
ayrı bir anlaşma gerekiyor), bu yüzden
v0.1.x bir kurulum o sınırın ötesine asla sessizce yükseltilmez — yeni şartlar
önce gösterilir ve sürümün releases sayfasından elle kurulması gerekir.

CLI:

```sh
sutura model.stl            # model_fixed.stl yazar
sutura model.obj            # model_fixed.obj yazar
sutura model.3mf -o fixed.3mf
sutura model.stl --human    # insanın okuyabileceği rapor
sutura model.stl --human --defects   # ayrıca girdi deliklerini / non-manifold bölgeleri listeler
sutura model.stl --human --diff      # ayrıca önce/sonra geometri farkını yazdırır
sutura model.stl --mode aggressive   # agresif onarım modunu kullan
sutura model.stl --classifier-engine classic  # classic sınıflandırıcı motorunu zorla
sutura model.stl --max-geometry-change 10 --max-risk 40   # onarım bütçeleri (aşağıya bakın)
sutura model.stl --max-risk 30 --force                    # bütçe aşılsa da sorunsuz kaydet
sutura validate model.stl   # onarmadan ANALİZ (salt-okunur rapor)
sutura model.stl --dry-run  # onarımın ne yapacağını raporlar, HİÇBİR ŞEY yazmaz
sutura a.stl b.3mf c.stl    # batch: her dosya bir _fixed çıktı alır
sutura --version            # sürümü yazdırır ve çıkar
```

OBJ notu: malzemeler/dokular (`mtllib`/`usemtl`) onarılmış çıktıda **korunmaz** —
mesh yeniden kurulur (yalnızca köşeler + üçgenler), bu yüzden doku koordinatları
taşınamaz. Girdi OBJ bunlara referans veriyorsa rapor bunu `material_discarded`
(JSON) ve `Material:` satırıyla (`--human`) açıkça belirtir. 3D dilimleyiciler
OBJ malzemelerini yok sayar, yani bu yalnızca onarılmış OBJ'yi doku işleme için
saklarsan önemlidir.

**Validate (`validate`).** `sutura validate model.stl` bir mesh'i onarmadan
analiz eder ve raporlar — salt-okunur bir sağlık kontrolü, hiçbir şey yazmaz.
JSON raporu `validation` taşır (`holes` / `non_manifold` listeleri merkez +
çap / yüz sayılarıyla, `self_intersections`, `connected_components`,
`watertight`, `signed_volume`, `surface_area`, `orientation`) ayrıca mesh
sınıflandırıcısından `detected_type` ve `detected_confidence`.
`--human` bunu okunur biçimde yazdırır (`--defects` her kusur bölgesini
listeler). Çok nesneli 3MF dosyaları nesne nesne doğrulanır
(`object_reports`). Çıkış 0, analizin çalıştığı anlamına gelir (kırık bir
mesh'te bile); kayıp / bozuk girdi 1 ile çıkar.

**Dry-run (`--dry-run`).** `sutura model.stl --dry-run`, bir onarımın NE
yapacağını yapmadan söyler: tespit edilen tür, çözülen onarım modu ve birebir
Aşama 1 eşikleri (`mincomponentsize` / `maxholesize`), `tuning_applied` ve
girdide bulunanlar (`holes_found` / en büyük delik çapı /
`non_manifold_regions` / `self_intersections` / `debris_faces_removable`)
ayrıca aşama 2'nin çalışıp çalışmayacağı. **Hiç çıktı dosyası yazmaz** —
ne `_fixed` ne geçici kalıntı. Eşikler, gerçek onarımın kullandığı aynı
`resolve_mode_params`'tan gelir, böylece ikisi asla ayrışamaz.

**Onarım modu (`--mode`).** Aşama 1 eşikleri için beş kademeli bir agresiflik
merdiveni; varsayılan **`auto`**'dur (tarihsel davranış: mesh sınıflandırıcı +
güven eşiği tür başına eşikleri seçer). Sabit modlar sınıflandırıcıyı atlar ve
birebir eşikleri kullanır:

| Mod | `mincomponentsize` | `maxholesize` | Etki |
|---|---|---|---|
| `low` | 8 | 200 | en muhafazakâr: yalnızca küçük delikler kapanır, minimum döküntü temizliği |
| `medium` | 8 | 1000 | tarihsel varsayılan eşikler |
| `auto` | — | — | sınıflandırıcı + güven eşiği (varsayılan); türe bağlı, sabit değil |
| `aggressive` | 12 | 3000 | daha çok döküntü silinir, daha büyük delikler kapanır |
| `extreme` | 20 | 10000 | en agresif; not: tek bağlı parçası 20'den az yüzlü bir nesnenin tamamı silinebilir — bu olduğunda rapor ayrı bir hata taşır (`category=error`, `extreme_removed_object` kodu), genel bozuk-girdi hatası değil. Extreme eşiklerine ek olarak **ek self-intersection geçişleri** çalıştırır: kendisiyle-kesişen yüzler silinir ve silme sonrası açığa çıkan kusurları yakalamak için Aşama 1 zinciri (delik kapatma + döküntü temizleme) bir kez daha çalışır. Bilinçli olarak tam bir remesh **değildir** (`meshing_isotropic_explicit_remeshing` kapsam dışıdır — topolojiyi öngörülemez şekilde değiştirebilir). |

Seçilen mod her zaman raporlanır (`repair_mode` JSON'da, `Mode:` `--human`da;
çok nesneli 3MF'de nesne başına). `extreme` için rapor ayrıca
`extreme_passes_applied` (ek geçişler çalıştıysa true) ve çalıştıysa
`self_intersections_found`/`self_intersections_removed` taşır; `--human` bir
"Extreme passes" satırı gösterir. GUI, aynı beş modu **Mod** düğmesi üzerinden
sunar (batch geneli; aşağıdaki GUI bölümüne bakın).

**Onarım profili (`--profile`).** Girdinin türünü bildiğinizde kullanışlı,
adlandırılmış, tercihe bağlı bir eşik preseti. Yalnızca mod `auto` iken
etkilidir — açık bir sabit mod her zaman kazanır (daha spesifik agresiflik
kontrolüdür). Sınıflandırıcı yine çalışır (tür/güven raporda kalır) ama
eşikleri sürmez:

| Profil | `mincomponentsize` | `maxholesize` | Ne zaman |
|---|---|---|---|
| `mechanical` | 8 | 300 | hassas/mekanik parçalar (ölçülü delik dolgusu) |
| `organic` | 12 | 1000 | düzgün/organik modeller (döküntü temizliği, büyük bölgeler) |
| `scan` | 4 | 10000 | tarama mesh'leri: agresif döküntü + büyük delik dolgusu |
| `miniature` | 1 | 50 | korumak istediğiniz minik parçalar (opt-in: varsayılan yol `>= 8` tutar) |
| `fast` | 8 | 200 | yalnızca küçük delikleri kapatan hızlı bir geçiş |

Seçilen profil raporlanır (`repair_profile` JSON'da, `Profile:` `--human`da).
Varsayılan (no `--profile`) eskisiyle bayt-birebir aynıdır. GUI, aynı
profilleri bir **Profil** açılır listesiyle sunar (batch geneli).

**Birim uyarısı.** STL/OBJ dosyaları birim meta verisi taşımaz ve boyut
temelli onarım eşikleri (delik boyutu, döküntü kesimi) milimetreyi varsayar.
Bir mesh yüklendikten sonra Sutura bounding box'unu hesaplar ve boyutlar
yalnızca inç veya santimetreden milimetreye ölçeklendiğinde makul görünüyorsa,
modelin mm olmayabileceğini söyleyen ENGELLEYİCİ OLMAYAN bir uyarı yayımlar
(`unit_warning` + `unit_hint` JSON'da, `--human`'da bir `WARNING:` satırı,
GUI'de görünür bir uyarı) — yazdırmadan önce ölçeği doğrulayın. 3MF dosyaları
birimlerini `<model unit="...">` özniteliğinde bildirir: bildirilen bir
inç/santimetre birimi aynen raporlanır, bildirilen milimetre birimi (spec
varsayılanı) güvenilir ve sezgisel kural bastırılır. Uyarı onarımın kendisini
asla değiştirmez.

**Onarım bütçesi (`--max-geometry-change` / `--max-risk` / `--force`).** Bir
onarımın mesh'i ne kadar değiştirmesine izin verileceğine dair iki isteğe
bağlı güvenlik sınırı; ikisi de onarımın zaten hesapladığı metrikleri kullanır:
- `--max-geometry-change PCT` — gerçek geometri değişimi
  `max(|hacim değişimi %|, |yüzey alanı değişimi %|)`'dir; bütçe bu değerin
  altındaysa aşılmıştır.
- `--max-risk SCORE` — `repair_risk` skoru (0–100); bütçe gerçek skorun
  altındaysa aşılmıştır.
- `0` (veya bayrağın verilmemesi) bir bütçeyi devre dışı bırakır; varsayılan
  hiç bütçe yoktur, yani mevcut davranış değişmez.

Bütçe aşıldığında çıktı **asla sessizce kaydedilmez**: etkileşimli bir
terminalde size sorulur (`Yine de kaydedilsin mi? [y/N]`), etkileşimli
olmayan çalıştırmalar (GUI alt süreci, betikler, dosya yöneticisi
entegrasyonu) kaydetmeyi reddeder ve çıkış kodu 1 olur. Rapor o zaman açık,
üst düzey `"status": "budget_declined"` işaretini (artı gerçek sayıları ve
bütçeleri içeren bir `budget` bloğu ve `budget_exceeded` sorun kodunu) taşır
— genel bir onarım hatasından ayrı, makine tarafından algılanabilir bir
sonuç. `--force` sormadan kaydeder (bütçe yine raporlanır). Onarım bütçe
içindeyse kayıt normal sürer ve `budget` bloğu gerçek sayıları bildirir.
Çok nesneli 3MF'de en KÖTÜ nesne karşılaştırmayı belirler. GUI aynı iki
bütçeyi onarım seçenekleri iletişim kutusunda sunar ve batch'ten sonra,
açık bir onayın ardından bütçe-reddedilen dosyaları `--force` ile yeniden
çalıştırmayı önerir.

Her onarım raporu ayrıca bir **onarım güven skoru** taşır: aşama 2 sonucunu,
kalan delikleri, sınıflandırıcı güvenini, eşik ayarını, onarım modunu,
self-intersection'ları ve hacim değişimini birleştiren tek bir 0–100 değer
(`repair_confidence` JSON'da) ve Yüksek/Orta/Düşük etiketi; `--human` bunu bir
`Confidence: X/100 (Label)` satırı olarak gösterir. Salt-okunur modlar bunun
yerine bir tahmin bildirir (onarım sonrası sinyaller henüz bilinmediği için):
validate ve `--dry-run` `estimated_confidence` taşır ve
`Estimated confidence: X/100 (Label) — actual result may differ after repair`
yazdırır.

Her onarım raporu ayrıca **Onarım Sağlığı / Onarım Riski** taşır — klasik güven
skorundan bağımsız, ayrı bir skorlama sistemi (`sutura/repair_score_config.json`
ile yapılandırılabilir):
- `repair_health` (0–100) — final mesh'in geometrik sağlamlığı: su geçirmezlik
  (aşama 2 doğruladı), non-manifold kenar yok, self-intersection yok, kalan
  delik yok faktörlerinin ağırlıklı toplamı.
- `repair_risk` (0–100) — onarımın mesh'i ne kadar değiştirdiği: yüz/vertex
  delta %, bileşen değişimi ve hacim delta %.
- `repair_status` / `repair_status_code` — Sağlık/Risk tier kombinasyonundan
  bir config lookup tablosuyla türetilen iki-eksenli etiket: `safe`
  ("Kontrol için güvenli"), `review` ("İnceleme önerilir"), `caution`
  ("Dikkatli olunmalı"), `failed` ("Başarısız / incele") veya `unavailable`.
  Sağlık ve Risk bağımsız eksenlerdir; yüksek-sağlık + düşük-risk (`safe`) bir
  onarım, orta-sağlık + yüksek-risk (`caution`) ile asla aynı statüye
  çökmez. Faktör başına katkılar da raporlanır
  (`repair_health_factors` / `repair_risk_factors`).

`--human` bunları `Health: X/100   Risk: Y/100   Status: <label>` olarak
gösterir. Skorlama fail-silent'tir: eksik/geçersiz metrik atlanır (ağırlığı
diğerlerine dağıtılır) ve skorlama onarımı asla bozmaz. Çok nesneli 3MF
raporları bunları nesne başına taşır. GUI kusur paneli seçili dosyanın başlık
satırında gösterir.

Birden çok dosyada her girdi sırayla onarılır ve bir özet yazdırılır (`N
su geçirmez, M uyarılı, K başarısız`), oluşan uyarı/hata türlerinin dökümüyle
(hacim değişimi, Stage 2 atlandı, kısmi onarım, bozuk girdi, extreme mod nesne silme). Herhangi bir
dosya başarısız olursa çıkış kodu sıfır değildir. JSON modda her dosyanın
raporu ayrıca bir `category` (`watertight`/`warning`/`error`) ve `issues`
listesi taşır ve batch özeti `summary.issue_counts` kazanır. `-o` yalnızca tek
dosyayla geçerlidir. JSON modda her dosyanın raporu ayrıca girdinin deliklerini
(merkez, çap) ve non-manifold bölgelerini anlatan bir `defects` listesi içerir;
`--human` modda bu liste yalnızca `--defects` verildiğinde gösterilir, böylece
varsayılan rapor kısa kalır. Çap değerleri, yaygın STL/3MF kuralı olarak
milimetreyi varsayar; dosyanız farklı bir birim kullanıyorsa yorumu buna göre
ölçeklendirin (Sutura ayrıca muhtemel mm-olmayan modelleri bir birim uyarısıyla
işaretler — yukarıdaki Birim uyarısı bölümüne bakın).

Her rapor ayrıca önce/sonra geometrisini `stage1` içinde kaydeder:
`volume_change_percent` (işaretli), `surface_area_before`/`after` ve
`surface_area_change_percent` ile `vertices_before`/`after`,
`faces_before`/`after`. Bunlar her zaman JSON'da bulunur ve çok nesneli 3MF
dosyalarında nesne başına gösterilir; `--human --diff` bunları da yazdırır ve
GUI kusur paneli tek satırlık bir özet gösterir ("Volume: +0.12% · Surface:
-2.37% · Vertex: 12→9").

Bir mesh yalnızca aşama 2 gerçekten çalışıp kapalı katıyı doğruladığında
**su geçirmez** sayılır. Aşama 1 bir meshi kapatır ama aşama 2 atlanır, hata
verir veya hiç çalışmazsa (ör. macOS/conda yerinde geri dönüşün kullanılamaması)
dosya su geçirmez değil, uyarı olarak raporlanır.

Çok nesneli 3MF dosyaları yerel olarak işlenir: her nesne meshi bağımsız
onarılır ve arşive geri yazılır, böylece hiçbir nesne kaybolmaz. Aşama 1'in
kapattığı (iki-manifold, kalan delik yok) her nesne, tek-mesh dosyalarıyla aynı
aşama 2 yardımcısından geçer, böylece manifold3d su geçirmez yeniden kurmasını
ve nesne başına `stage2` raporunu alır. Rapor sonucu nesne başına listeler
(kalan delik, iki-manifold, aşama 2 kararı) ve `objects_watertight` /
`objects_stage2_ok` özetlerini taşır; dosyanın kategorisi TÜM nesnelerden
türetilir, bu yüzden tek bir hâlâ-açık nesne tüm dosyanın su geçirmez
raporlanmasını engeller. Kusurlar da nesne başına hesaplanır
(`object_reports[i].defects`); 3MF için üst düzey toplu bir `defects` alanı
yoktur. Bayt bayt özdeş geometriye sahip nesneler tek onarımı paylaşır ama her
biri yine kendi nesne başına raporunu alır (aşama 2 sonucu geometrinin saf bir
fonksiyonudur, bu yüzden önbellekteki nesnenin raporu kopyalar için de
geçerlidir).

GUI:

```sh
~/.local/share/sutura/gui.py
```

GUI, batch onarımı destekler: istediğiniz kadar dosya ekleyin, **Onar**'a basın
ve her biri sırayla işlenir, sonuç dosya başına listelenir. Batch bittiğinde
log'un üstünde bir özet şeridi belirir (`X su geçirmez, Y uyarılı, Z
başarısız`); bu şerit, uyarı/hata türlerini ve her birinin kaç dosyayı
etkilediğini listeleyen tıklanabilir bir **sorunları göster** bağlantısı içerir.
Bir dosya seçtiğinizde girdi kusurları (çapıyla delikler ve non-manifold
bölgeler) log'un altındaki bir panelde, batch özet şeridinden ayrı gösterilir.
Dosyalar **Dosya ekle…** (yerel çoklu seçim, lastik bant dahil), **Klasör
ekle…** (klasördeki her `.stl`/`.3mf`, tek seviye) veya dosya/klasörleri
pencereye sürükleyerek eklenebilir. **Durdur** çalışan onarımı sonlandırır ve
kalan dosyaları iptal edilmiş olarak işaretler. Sürükle & bırak, yerel Wayland
oturumlarında çalışır (GUI bir Qt uygulamasıdır, XWayland değildir). GUI,
kendi koyu Fusion temasıyla gelir (teal vurgu rengi), böylece sistem masaüstü
temasından bağımsız olarak her platformda ve Qt sürümünde aynı görünür.

#### Onarım öncesi analiz

![Onarım öncesi analiz](assets/analyze-panel.png)

Onarımdan önce **Analiz Et**, CLI'daki `validate` ve `--dry-run` ile aynı
salt-okunur kontrolleri her dosya için hiçbir şey yazmadan çalıştırır:
tespit edilen türü ve onarım modunu, eşik ayarını, girdinin delik /
self-intersection / non-manifold / döküntü sayılarını, watertight ön-kararını
ve bir **tahmini güven** değerini bildirir
(`Estimated confidence: X/100 (Label) — actual result may differ after
repair`, çünkü onarım sonrası sinyaller henüz bilinmiyor). Altında birkaç
**mod önerisi** listeler (ör. tarama kaynaklı delikler için agresif/extreme
adımı, girdide en az bir delik olduğunda "mevcut mod yeterli olabilir ama
tatmin etmezse bir üst mod denenebilir" yönünde yumuşak bir ipucu, mod hâlâ
düşük seviyedeyken bir üst mod denenebileceğine dair
düşük-güven ipucu veya extreme'un küçük parçaları silebileceğine dair bir
çekince). Öneriler **yalnızca bilgilendirme amaçlıdır ve modu asla otomatik
değiştirmez — karar kullanıcıda kalır.** Sezgisel kural tetiklenirse engelleyici
olmayan bir birim uyarısı (model inç/cm cinsinden olabilir) da gösterilir.

#### Kusur detay paneli

![Kusur detay paneli](assets/defect-panel.png)

Bir dosya seçildiğinde log'un altındaki panel, girdi meshinde bulunan
kusurları listeler: her deliğin merkezi ve çapı (mm) ve her non-manifold
bölge. Bu, log'un üstündeki batch özet şeridini tamamlar — şerit batch başına
bir sayımdır, bu panel dosya başına detaydır. Onarımdan sonra panel başlığı
ayrıca sonucun güvenini `Confidence: X/100 — Yüksek/Orta/Düşük` segmenti
olarak gösterir. Model milimetre cinsinden olmayabilirse, log'da ve bu panelin
başında engelleyici olmayan bir birim uyarısı görünür (yukarıdaki Birim
uyarısı bölümüne bakın).

**Kusur ısı haritası.** Kusur listesinin altında **Isı haritası göster**,
seçili meshi kusur bölgeleri (delik kenarları ve non-manifold alanlar) gri
mesh üzerinde kırmızı vurgulanmış olarak çizer ve küçük bir önizleme olarak
gösterir. Önizlemeye tıklamak daha büyük bir yakınlaştırma diyaloğu açar.
Çizim isteğe bağlıdır (asla otomatik değildir; böylece büyük bir batch
takılıp kalmaz) ve dosya başına önbelleklenir. Çok nesneli bir 3MF'de, kusur
panelinin mevcut ilk-nesneyi-göster davranışıyla tutarlı olarak ilk nesne
çizilir. Çizim, bir CPU rasterizer'ıyla yapılır (headless, AppImage ve macOS
CI'de çalışır) ve GUI'nin duyarlı ve çökmesiz kalması için bir alt süreçte
çalışır; bir mesh çizilemezse sessizce salt metin panele geri dönülür.

**Öncesi/sonrası karşılaştırma.** Bir dosya onarıldıktan sonra
**Öncesi/sonrası göster**, orijinal ve onarılmış meshleri *aynı* kamera
çerçevesiyle çizer ve ana görüntü, **Orijinal**/**Onarılmış** arasında geçiş
yapan bir düğme ve ana görüntünün altında *en yoğun orijinal bozukluk
bölgesinin* (en büyük fiziksel köşegen uzunluğuna sahip kusurun) daha küçük
bir **detay** yakın çekimi içeren bir diyalog açar. Kamera otomatik olarak o
en yoğun kusura yönlendirilir, böylece kusur asla mesh'in arkasında kalmaz;
yalnızca kusursuz bir mesh (veya bbox merkezine oturan bir kusur) sabit
izometrik görünüme düşer. Orijinal görünüm
kusurlarını kırmızıyla işaretler. Onarılmış görünüm üç durumlu bir renk
haritası kullanır: hiç bozulmamış bölgeler **gri**, eski bir kusurun olduğu
yer artık sağlıklıysa parlak **yeşil** `(46,204,113)`, hâlâ kusur kalıyorsa
**turuncu** `(255,140,60)`. Onarım topolojiyi değiştirdiği için (önceki/
sonraki vertex indeksleri eşleşmez) yeşil sınıflandırma *uzaysaldır*: her
onarılmış yüz, orijinal kusur merkezlerine göre ölçülür (`defects.detect`'in
gerçek yarıçapı) ve yalnızca en büyük kusurlar (en fazla 256) vurguyu
üretir — binlerce mikro çatlaklı tarama mesh'lerinde hızlı kalır. Yakın
çekim, her iki taraf için aynı yakınlaştırılmış
kameralı çerçeveyi kullanır, böylece orijinal/onarılmış karşılaştırması
birebir tutarlıdır. Isı haritasıyla aynı CPU-çizici kısıtı yüzünden bilinçli
olarak etkileşimli bir 3D kaydırıcı değil, statik bir tıkla-geçiştir; alt
süreçte çalışır ve isteğe bağlıdır, bu yüzden bir batch'i asla yavaşlatmaz.

![Öncesi/sonrası karşılaştırma diyaloğu](assets/before-after-panel.png)

Diyalogun üstündeki **Statik / İnteraktif** anahtarı, etkileşimli 3D
görünümü ekler (ilk kez seçildiğinde tembel üretilir, sonra diyaloğun ömrü
boyunca önbellekte tutulur): **sürükleyerek** mesh döndürülür, **fare
tekerleğiyle** yakınlaştırılır. Sürükleme sırasında düşük-poligonlu bir LOD
arka plan iş parçacığında her karede çizilir (en yeni kare kazanır, GUI
sürecinde pymeshlab yoktur); durduktan ~300 ms sonra tam çözünürlüklü bir
kare çizilir. İnteraktif görünüm, statik karşılaştırmayla aynı kusura dönük
kamerayla açılır. İkinci bir anahtar onarılmış görünümü **Onarım durumu**
renk haritası ile **Yüzey sapması** haritası arasında değiştirir: her yüz,
onarılmış yüzeyin orijinal yüzeye uzaklığına göre renklendirilir (nicemle
ölçekli rampa, lacivert → camgöbeği → sarı → kırmızı); hâlâ bozuk kusurlar
üstte turuncu çizilir ve global maksimum sapma (Hausdorff mesafesi)
diyalogda gösterilir.

**Onarım modu.** Isı haritası/öncesi-sonrası düğmelerinin yanındaki
**Mod: Otomatik** düğmesi, beş kademeli bir kaydırıcı içeren küçük bir diyalog
açar — **Düşük / Orta / Otomatik / Agresif / Aşırı** — kaydırıcı hareket
ettikçe canlı güncellenen tek cümlelik bir açıklamayla (Aşırı kademesi,
20'den az yüzlü nesneleri silebileceği konusunda dürüstçe uyarır). Mod,
**batch geneli** bir ayardır: sonraki Onar çalışmasında tüm dosyalara uygulanır
(dosya başına değil) ve CLI'ya `--mode <mod>` olarak iletilir (CLI bayrağıyla
aynı beş değer, varsayılan `auto`). Aynı diyalog ayrıca iki **onarım bütçesini**
(maks. geometri değişimi %, maks. onarım riski 0–100; 0 = sınır yok) taşır.
Güncel mod her zaman düğmede görünür.

Bir onarım bir bütçeyi aştığında CLI kaydetmeyi reddeder (çıktı dosyası
yazılmaz) ve `"status": "budget_declined"` bildirir; batch'ten sonra GUI,
etkilenen dosyaları ve gerçek sayılarını listeleyen bir onay diyaloğu gösterir;
**Yine de kaydedilsin mi?** tam olarak bu dosyaları `--force` ile yeniden
çalıştırır (reddedilen satırlar zorlanmış sonuçlarla güncellenir).

Dolphin: bir STL/OBJ/3MF dosyasına sağ tık -> **Sutura ile Onar**. Tek seçimde GUI
dosya yüklü olarak açılır; çoklu seçimde her dosya arka planda onarılır ve bir
özet diyaloğu gösterilir.

Servis menüsünü kurduktan veya kaldırdıktan sonra `kbuildsycoca6` çalıştırın
(kurulum bunu otomatik yapar) veya Dolphin'i yeniden başlatın.

### OrcaSlicer eklentisi (deneysel)

Ayrıca `orcaslicer-plugin/` altında, kurulu Sutura CLI'sını ayrı bir süreç
olarak çağırarak bir dosyayı doğrudan dilimleyiciden onaran **deneysel** bir
[OrcaSlicer betik eklentisi](orcaslicer-plugin/) vardır. Bir başlangıç noktası
olarak sunulur ve **gerçek bir OrcaSlicer'da test edilmemiştir**: hedeflediği
Python eklenti sistemi yalnızca OrcaSlicer **nightly sürümlerinde / 2.4.2'den
yeni sürümlerinde** bulunur ve biz bu sürümleri çalıştırmadığımız için uçtan
uca doğrulayamadık. Kurulum adımları ve sınırlamaları için
[eklenti README'sine](orcaslicer-plugin/README.md) bakın.

## Mesh türüne duyarlı onarım

Sutura, bir girdi meshinin **mekanik** mi (küp, dişli, CAD parçası) yoksa
**organik** mi (heykel, taranmış model) olduğunu salt geometriden — komşu
yüzlerin dihedral açıları, numpy ile hesaplanır — sezgisel olarak tahmin eder.
Bu bir ML modeli *değildir* ve bilinçli olarak muhafazakârdır: yalnızca yüksek
güvenli durumlarda harekete geçer, aksi halde `unknown` bildirir ve bu durumda
tarihsel varsayılan Aşama 1 parametreleri olduğu gibi kullanılır.

Güven skoru bir **işaretli-marj (signed-margin) değeridir**: bitişik yüz
açılarından üç dihedral bandı hesaplanır — `near90` (`[60,120]°`, keskin
kenarlar), `flat` (`<1°`, gerçek düz yüzeyler) ve `gentle` (`[1,15)°`, hafif
eğrilik) — ve her biri yumuşak bir sigmoidden geçirilir. Bantlar şöyle
birleştirilir: **mekanik = maks(near90 sinyali, flat sinyali)** (birinin
yeterli olması) ve **organik = min(düşük near90, düşük flat, yüksek gentle)**
(üçünün birden sağlanması); böylece karar, tek bir sert eşik yerine yumuşak
bir marjdır — `[55,60]` near90 sınırında keskin bir sıçrama yoktur.
`flat`/`gentle` ayrımı, yüksek poligonlu pürüzsüz organik mesh'lerin mekanik
okunmasını engelleyen şeydir: dihedral açıları ~2–5°'dir, yani `flat` değil
`gentle` bandına düşer. Bir `unknown` sonucu, düz bir 0 yerine yine de
sıfırdan farklı bir yakınlık değeri taşır (meshin hangi sınıfa yaklaştığı ve
ne kadar yakın olduğu); böylece geri dönüş bile bilgilendiricidir.

Tespit edilen tür, GUI kusur panelinin başlığında (ör. `Tespit edilen:
mechanical (0.92)`) ve `--human` raporunda bir `Type:` satırı olarak
gösterilir; JSON raporu `detected_type` ve `detected_confidence` taşır. Bir
kalibrasyon aracı (`scripts/calibrate_classifier.py`), etiketli sentetik bir
kümeye (`tests/make_classifier_set.py`) karşı precision/recall ve güven
skoru ayrışımını ölçer; böylece eşikler kontrol edilebilir ve geri alınabilir
kalır.

Sınıflandırıldığında tür iki Aşama 1 eşiğini ayarlar:

| Tür | `mincomponentsize` (döküntü eşiği) | `maxholesize` (delik dolgusu) | Etki |
|---|---|---|---|
| mekanik | 8 | 300 | küçük keskin detayları korur, aşırı büyük delik yamalarından kaçınır |
| organik | 12 | 1000 | tarama döküntüsünü daha agresif atar, büyük açık bölgeleri kapatır |
| unknown | 8 | 1000 | tarihsel varsayılanlar (değişmez) |

> Bu tür başına değerler **deneysel başlangıç noktalarıdır**, gerçek onarım
> verisiyle kalibre edilmemiştir — ihtiyatlı, geri alınabilir bir seçimdir.
> Yalnızca yukarıdaki iki eşik kayar; daha fazla örnek toplandıkça
> `repair.py`'de ayarlanabilirler.

Sınıflandırılan bir mesh, bu ayarlanmış eşikleri yalnızca confidence'ı bir
**sınıf-özel eşiği** aştığında alır (mekanik ≥ 0.75, organik ≥ 0.70). Eşiğin
altında tür yine raporlanır (`detected_type`), ama onun yerine ihtiyatlı
varsayılan eşikler kullanılır (`mincomponentsize=8`, `maxholesize=1000`);
rapor ve GUI kusur paneli bunu `tuning_applied: false` / "varsayılan eşikler"
olarak gösterir. (Organik eşik eskiden daha düşüktü çünkü bir sınıflandırıcı
bug'ı organik confidence'ı ~0.62'de tavanlanmış gibi gösteriyordu; bu bug
düzeltildi ve güven artık tavanlı değil, dolayısıyla her iki eşik de
kalibrasyon setindeki en zayıf doğru tahminin hemen üzerinde.)

### Sınıflandırıcının bilinen sınırlaması

Eğrisel ama mekanik parçalar (ör. bir silindir, mil veya yuvarlatılmış
geometri) classic motor tarafından sınıflandırılmaz — `unknown` kovasına düşer
ve varsayılan parametreleri korur. Bu classic'in bilinçli bir ödünleşimidir:
yalnızca açıkça düz/keskin mekanik veya açıkça pürüzsüz organik meshlerde
devreye girer ve yanlış bir parametre seti uygulamaktansa hiçbir şey yapmamayı
tercih eder. **Varsayılan (experimental) motor artık bu şekilleri işler**:
eğrilik gelişebilirlik sinyali (normaları ortak bir büyük daire üzerinde
yatan yüzey alanı oranı — silindir/yuvarlatma/borunun geometrik imzası) artı
eğitilmiş başlık, silindirleri, tüpleri ve yuvarlatmaları hasarlı varyantları
dahil mekanik olarak sınıflandırır. Gerçekten çift-kavisli bir mekanik parça
(bükülmüş bir boru) hâlâ ıskalanır — geometrik olarak silindirden çok torusa
benzer.

### Sınıflandırıcı motorları (`--classifier-engine`)

**experimental** motor (`sutura/mesh_classifier_v2.py`) artık **varsayılandır**:
etiketli sentetik sette ve 40 mesh'lik gerçek dünya korpusunda classic
sezgiselden ölçülebilir biçimde iyidir: 36 etiketli gerçek örnekte **29/36**'ya
karşı classic **16/36** (mekanik isabet **16/20**'ye karşı **2/20** — classic
tarayıcı yanlılığı genişletilmiş korpusta bariz; organik 13/16'ya karşı 14/16)
ve 71 mesh'lik etiketli sette (35 sentetik + 36 gerçek) LOO-CV 0.845'e karşı
classic 0.648. Orijinal
**classic** motor (`mesh_classifier`) kullanılabilir kalır — `--classifier-engine
classic` bayrağıyla veya `SUTURA_CLASSIFIER_ENGINE=classic` ortam değişkeniyle
seçilir (onarım, validate ve `--dry-run` için aynı şekilde geçerlidir). Her iki
motor da fiilen kullanılanı `classifier_engine` (JSON) ve bir `Classifier:`
satırı (`--human`) olarak bildirir.

Experimental motor, classic özelliklere üç şey ekler:

1. **RANSAC düzlem segmentasyonu** — mesh alanının ≥%1'ini kaplayan sağlam
   düzlemsel yamalar bir RANSAC döngüsüyle tespit edilir (örnek birim: yüz
   merkezi + normali); yama **sayısı** ve **alan oranı** özelliklere katılır.
2. **Eğrilik gelişebilirlik sinyali** (`developable_fraction`) — normaleri
   ortak bir büyük daire üzerinde yatan yüzey alanı oranı; sıfır-Gauss-eğriliği
   (gelişebilir) yüzeylerin geometrik imzası: silindirler, tüpler, yuvarlatmalar
   ve diğer doğrusal kavisli mekanik parçalar. Çift-kavisli organik formlar
   (küre, blob, torus) herhangi bir tek eksene yalnızca ince bir bantta diktir,
   bu yüzden düşük puan alır. Bu, yalnızca düzlem gören bir RANSAC'ın
   göremediği şeydir — silindirin düzlemsel yaması yoktur.
3. **Küçük eğitilmiş lojistik regresyon başlığı** (saf numpy)
   `[near90, flat, gentle, plane_count, plane_area, developable_fraction]`
   üzerinde; 35 mesh'lik sentetik set + 40 mesh'lik gerçek dünya korpusunun 36
   etiketli meshiyle
   (`tests/real-world-samples/`, mekanik-ama-organik taramalar doğru
   etiketlenmiş) eğitilir. Ağırlıklar gömülüdür; eğitim + leave-one-out çapraz
   doğrulaması `scripts/train_classifier_v2.py`'de yaşar.

Uyarılar: eğitilmiş başlık ~71 etiketli mesh üzerinde zayıf bir sinyaldir
(LOO-CV 0.845 — fark beklenir) ve kendinden emin ama yanlış bir experimental
tahmini classic'e karşı yeniden kontrol edilmez (yalnızca sınır-yakını durum
kontrol edilir: başlık yazı-tura olduğunda — `|p_mech − 0.5|·2 < 0.15` — ve
classic seçilen sınıfla güçlü biçimde hemfikirse, başlık sınıfını korur ama
classic'in güvenini bildirir, böylece ince ayar eşiği güçlü sinyali görür).
Experimental motor çökerse veya geçersiz bir şey döndürürse stderr'de
bir uyarıyla sessizce classic'e düşer, böylece bozuk bir v2 onarımı
çökertmek yerine classic'e geriler.

v2 motoru `sutura/mesh_classifier_v2.py`'de classic motorun birebir kopyası
artı eklemeler olarak durur, böylece classic modülü el değmeden kalır.
`scripts/calibrate_classifier.py` (varsayılan motor = experimental) ve
`scripts/compare_classifier_engines.py` ikisini de aynı setlerde karşılaştırır;
`tests/test_mesh_classifier_v2.py` değişmezleri korur (yalnızca stdlib+numpy,
başlık devre dışıyken classic geri dönüşü, sentetik set %100).

### Sınıflandırıcı metodolojisi

Experimental motorun özellikleri:

| Özellik | Kaynak | Ne yakalar |
|---|---|---|
| `near90` | dihedral istatistikleri (numpy) | keskin `[60,120]°` kenarlar — mekanik |
| `flat` | dihedral istatistikleri | gerçek düzlemsel yüzeyler (`<1°`) |
| `gentle` | dihedral istatistikleri | hafif eğrilik `[1,15)°` — organik ipucu |
| `plane_count` | RANSAC düzlem segmentasyonu | sağlam düzlemsel yamaların sayısı |
| `plane_area` | RANSAC düzlem segmentasyonu | bu yamaların alan kesri |
| `developable_fraction` | eğrilik Gauss-haritası sinyali | ortak büyük çemberdeki normaller (silindir/boru/yuvarlatma) |

**Değerlendirilen aday — özdeğer şekil descriptor'ları (ENTEGRE EDİLMEDİ).**
**Weinmann, Jutzi & Mallet (2015)**'in — *"Feature relevance assessment for
the semantic interpretation of 3D point cloud data"*, ISPRS Annals of the
Photogrammetry, Remote Sensing and Spatial Information Sciences II-3/W5
(akademik atıf; formüller herkese açık standarttır, hiçbir uygulamadan kod
alınmadı) — standart nokta-kümesi özdeğer descriptor'larını test ettik. Bunlar
**köşe konumlarının** (normal değil) 3×3 kovaryans matrisinin
`λ1 ≥ λ2 ≥ λ3` özdeğerlerinden hesaplanır: `Linearity = (λ1−λ2)/λ1`,
`Planarity = (λ2−λ3)/λ1`, `Sphericity = λ3/λ1`,
`Omnivariance = (λ1·λ2·λ3)^(1/3)`, `Anisotropy = (λ1−λ3)/λ1`,
`Eigentropy = −Σ(λi/s·ln(λi/s))`, `Surface Variation = λ3/(λ1+λ2+λ3)`.
Hipotez şuydu: uzun/ince mekanik parçalar yüksek Linearity, yuvarlak organik
formlar yüksek Sphericity taşır — normal-tabanlı sinyallere dik bir bilgi.
Descriptor'lar numpy ile formüllerden uygulandı ve kanonik şekillerde
doğrulandı (küre → Sphericity≈1; 100×1×1 kutu → Linearity≈1; düz plaka →
Planarity≈1).

**Sonuç — descriptor'lar etiketli sette YARDIMCI OLMUYOR, bu yüzden ENTEGRE
EDİLMEDİ.** 71 etiketli mesh üzerinde (35 sentetik + 36 gerçek) bırak-bir-dışarı
(LOO-CV), eşik 0.50:

| Özellik seti | LOO-CV doğruluğu | mekanik | organik |
|---|---|---|---|
| temel 6 (mevcut) | **0.845** (60/71) | 32/39 | 28/32 |
| + `linearity`, `sphericity` | 0.831 (59/71) | 33/39 | 26/32 |
| + 7 descriptor'ın tümü | 0.817 (58/71) | 32/39 | 26/32 |

Sonuç 0.50–0.65 karar eşiklerinde sağlamdır. Mekanik isabet en fazla 1 kazanırken
organik isabet 2 kaybediyor — global özdeğer şekli, mevcut özelliklerle büyük
ölçüde örtüşüyor (başlık eğrilik/düzlemsellik yapısını zaten görüyor); bu yüzden
descriptor'lar net kazanç olmadan yeni hatalar üretiyor. "Yalnızca gerçekten
katkı sağlıyorsa entegre et" kuralı gereği dışarıda kalıyor; sınıflandırıcı kodu
bu değerlendirmeden değişmedi.

### Lokal roughness / geometrik / istatistiksel özellikler tie-breaker olarak (ENTEGRE EDİLMEDİ)

İzleme değerlendirmesi (FAZ 8): kaynak depolar baştan sona yeniden okundu ve
kalan özellik kategorileri **sıfırdan** akademik referanslarından yeniden
uygulandı (GPL-3.0 kod asla kopyalanmaz — yalnızca herkese açık formüller):
**roughness** (Gauss eğriliği roughness'u, Wang et al. 2012; Normal Farkı,
Ioannou et al. 2012; lokal yoğunluk, Rabbani et al. 2006 — lokal yoğunluk
entropisi N/A çünkü STL'de köşe rengi yok), **geometrik** (lokal yoğunluk, en
uzak mesafe, maksimum yükseklik, yükseklik std sapması, Blomley / Jutzi /
Weinmann 2016) ve **istatistiksel şekil dağılımı** (noktadan-merkeze mesafe,
ikili nokta mesafesi, sqrt üçgen alanı — D1/D2/D3). `timzhang642/3D-Machine-Learning`
deposu yeniden kontrol edildi: makale/ders **link listesi, kod yok**, bu yüzden
kullanılabilir bir referans uygulama eklemiyor.

**Metodoloji — tie-breaker, feature fusion değil.** FAZ 6'dan (yeni özellikleri
başlık vektörüne karıştırıp tüm corpus'ta LOO-CV) farklı olarak FAZ 8 önce
**sınır/uyuşmazlık alt-kümesini** belirledi — head güveni düşük (< 0.5) ya da
classic ile experimental motorların farklı gerçek sınıf tahmin ettiği mesh'ler —
ve yeni özellikleri **yalnızca bu alt-kümede ikincil bir sinyal olarak** ölçtü
(yeni lokal özellik özetleri üzerinde bırak-bir-dışarı lojistik). Amaçlanan rol
"motorlar emin değilken ekstra ipucu", varsayılan kararın değişmesi değil.

| Sınır alt-kümesi | n | Temel (head) | Tie-breaker (yeni özellikler, LOO) |
|---|---|---|---|
| düşük güven VEYA herhangi uyuşmazlık | 27 | 0.778 (21/27) | 0.741 (20/27) |
| düşük güven VEYA gerçek-sınıf uyuşmazlığı (rafine) | 14 | 0.643 (9/14) | 0.429 (6/14) |

Rafine alt-kümede özellik-başına doğruluk-etiketi korelasyonu en fazla |r| ≈ 0.36
(`gc_std`); hemen her özellik |r| ≤ 0.2'de. Tie-breaker sınır alt-kümesi
doğruluğunu **iyileştirmiyor** (0.778 → 0.741 ve 0.643 → 0.429) — ~14–27 örnek
ve 18 özellikle LOO lojistik aşırı uyum yapıyor ve hiçbir lokal özellik
gerçekten-belirsiz vakaları ayıracak kadar ayırma sinyali taşımıyor. **Karar:
ENTEGRE EDİLMEDİ** — `--experimental-tiebreaker-features` bayrağı ve GUI
toggle'ı yok. Varsayılan sınıflandırıcı değişmedi; bu, FAZ 6 ile aynı dürüstlük
çıtasında belgelenmiş bir negatif sonuçtur.

### MeshCNN kenar özellikleri fusion ve tie-breaker olarak (ENTEGRE EDİLMEDİ)

İzleme değerlendirmesi (FAZ 9): **MeshCNN** 5-B kenar-değişmez özelliği
(Hanoeka et al. 2019, MIT lisanslı; formülden temizce yeniden uygulandı, kod
kopyalanmadı) sıfırdan saf numpy ile implemente edildi. Reddedilen Weinmann
kovaryans descriptor'larından kavramsal olarak farklıdır: nokta-bulutu komşuluk
kovaryansı değil, mesh bağlantılılığına dayanan **kenar-başına** bir özellik.
Her iç kenar (tam iki üçgene ait) için: `dihedral` = iki komşu yüz düzlemi
arasındaki açı (`π − arccos(n₁·n₂)`), `symmetric_opposite_angles` (2 değer) =
her üçgende kenara bakan tepe açısı (sıralı) ve `symmetric_ratios` (2 değer) =
her üçgende tepe-yüksekliği/kenar-uzunluğu oranı (sıralı). 5 kenar-başına değer
**10 global istatistiğe** (her boyut için mean + std) özetlendi. Kanonik
şekillerde doğrulandı: küp (π/2 katlanmaları + eşdüzlemli köşegenler karışımı)
dihedral ort. ≈ 2.09, pürüzsüz küre ≈ 2.97 (π'ye yakın).

**Test A — feature fusion (FAZ 6 stili).** 71 etiketli mesh üzerinde LOO-CV:

| Özellik vektörü | LOO-CV doğruluğu | mekanik | organik |
|---|---|---|---|
| temel 6 (mevcut) | **0.845** (60/71) | 32/39 | 28/32 |
| temel 6 + 10 kenar özelliği | 0.831 (59/71) | 33/39 | 26/32 |

Fusion doğruluğu **kötüleştiriyor**. **Test B — tie-breaker (FAZ 8 stili)**, aynı
sınır/uyuşmazlık alt-kümesi tanımı:

| Sınır alt-kümesi | n | Temel (head) | Tie-breaker (kenar özellikleri) |
|---|---|---|---|
| düşük güven VEYA herhangi uyuşmazlık | 27 | 0.778 (21/27) | 0.741 (20/27, LOO lojistik) |
| düşük güven VEYA gerçek-sınıf uyuşmazlığı (rafine) | 14 | 0.643 (9/14) | 0.857 (12/14, LOO) — ama 3-fold×20 tekrar: tüm-10 = 0.733, en-iyi-3 = 0.902 |

Birleşim alt-kümesinde özellik-etiket korelasyonları |r| ≈ 0.44'e ulaşıyor
(`opp_angle_min_mean`); rafine sayılar umut verici görünüyor ama n = 14 "belirgin"
kanıt için fazla küçük, "en-iyi-3 özellik" tahmini seçim yanlılığı taşıyor ve
daha büyük (n = 27) birleşim alt-kümesi **iyileşme göstermiyor**. Test A temiz
bir negatif ve Test B sağlam bir kazanç değil, bu yüzden FAZ 6/FAZ 8 ile aynı
çıta uygulanıyor: **ENTEGRE EDİLMEDİ** — `--experimental-edge-features` bayrağı
ve GUI toggle'ı yok. Varsayılan sınıflandırıcı değişmedi; yukarıdaki kesin
sayılar dürüst kayıttır.

### Geriye dönük tek-özellik taraması (FAZ 10)

Maintainer'ın standing kuralı gereği (reddedilen deneylerde özellik-başına
korelasyonları her zaman rapor et), üç reddedilen aile (Weinmann FAZ 6, lokal
roughness/geometrik/istatistiksel FAZ 8, MeshCNN kenar FAZ 9) **tam 71-mesh
etiketli sette** (yalnızca sınır alt-kümeleri değil) yeniden tarandı. En güçlü
sinyaller **gerçek** — n=71 gürültü çıtasının (|r| ≈ 0.4) üstünde ve 30
bootstrap alt-örnekleminde işaret-sabit (std ≈ 0.04):

| Özellik | r (tam 71-set) | aile |
|---|---|---|
| `oppmax_std` (MeshCNN ratio-max std) | +0.718 | FAZ 9 |
| `don_mean` (Normal Farkı) | +0.673 | FAZ 8 |
| `dihed_mean` (MeshCNN dihedral ort.) | −0.633 | FAZ 9 |
| `rmin_mean` (MeshCNN ratio-min ort.) | +0.563 | FAZ 9 |
| `gc_mean` (Gauss eğriliği roughness) | +0.480 | FAZ 8 |

Ama **tek** özellik olarak başlığa neredeyse hiçbir şey katmıyorlar: tek başına
LOO-CV kazancı ≤ +0.007 ortalama doğruluk (5-fold×20; deterministik LOO 0.845 →
0.859 = bir mesh) ve son-model uyumu **sıfır** karar değişimi veriyor. Bu güçlü
korelasyonlar mevcut dihedral tabanlı özelliklerle (`near90`/`flat`/`gentle` +
`developable_fraction`) büyük ölçüde **örtüşüyor**. **Hiçbiri çok yüksek
tek-başına-entegrasyon barını geçmedi** — "ilginç alt-parça, yalnızca uygun bir
özellik-seçimi çerçevesiyle ya da yeni veriyle yeniden bakılmalı" olarak
işaretlendi, entegre edilmedi.

## Test

Sentetik kırık mesh:

```sh
python3 tests/make_broken_stl.py /tmp/broken.stl
sutura /tmp/broken.stl --human
```

Üretici; eksik bir yüzü, ters sarmalanmış bir yüzü, yinelenen bir yüzü, bir
yüzgeç üçgenini ve kendisiyle kesişen bir üçgeni olan bir küp üretir.

Regresyon süitleri:

```sh
python3 tests/make_layered_multiobject_3mf.py --check   # katmanlı çok nesneli 3MF
python3 tests/test_adversarial.py                       # bozuk girdi işleme
```

`tests/real-world-samples/` içindeki gerçek dünya örnekleri
[Thingi10K](https://ten-thousand-models.appspot.com/) veri kümesinden ve Artec
3D STL kataloğundan gelir (Zhou & Jacobson): toplam 40 mesh — mekanik, organik
ve birkaç gerçekten bozuk model. Orijinal lisanslarını korurlar (Faz 2
eklemeleri model başına CC BY / CC0; tam dosya-başına atıf için
`docs/ATTRIBUTION.md`'ye bakın) ve her birinden beklenen onarım sonucu
`tests/real-world-samples/README.md`'dedir.

### Benchmark corpus'u (115 mesh)

Büyük **onarım benchmark corpus'u** (52 Artec STL taraması + 63 Thingi10K
STL, sıkıştırılmamış ~6 GB — `docs/repair-benchmark-strict-watertight-2026-09.md`'nin
arkasındaki corpus) git deposunda **değildir** (depo boyutunu ~100× büyütür).
GitHub'daki **`benchmark-corpus-v1`** sürümünde parçalı bir `.tar.gz` olarak
yayımlanır. Şununla indirip doğrulayın:

```sh
scripts/fetch_benchmark_corpus.sh            # -> /tmp/sutura_corpus_100
scripts/fetch_benchmark_corpus.sh /some/dir  # özel hedef
```

Betik sürüm parçalarını indirir, SHA-256'larını doğrular, birleştirir ve
açar — böylece yeni bir makine (örneğin gelecekteki bir Linux kutusu)
Thingi10K'yı yeniden taramadan benchmark'ı yeniden üretebilir. Betik başlığı
ve `docs/ATTRIBUTION.md` kaynak kökenini ve yeni mesh'ler eklendiğinde
corpus'un nasıl yeniden paketlenip/yeniden yükleneceğini açıklar.

Benchmark harness'i (`scripts/benchmark_repair_corpus.py`) ayrıca **opsiyonel
bir manifold3d çapraz-doğrulama sütunu** taşır (FAZ 10): onarılan her mesh'in
nihai geometrisi, pymeshlab tabanlı `defects.detect()` sıkı kontrolünün yanında
manifold3d ile bağımsız olarak kontrol edilir (bir Manifold `Error.NoError` ile
kurulur ve boş değilse ⇒ su geçirmez) ve iki sonuç karşılaştırılır. Sıkı sonucu
ya da hattı asla değiştirmez — manifold3d yoksa sütun `n/a` bildirilir.
manifold3d opsiyonel/ağır bir bağımlılıktır (önceden derlenmiş wheel'ler yalnızca
Python 3.13'e kadar; `requirements-311.txt` ve macOS conda env'sinde zaten
bildiriliyor — `requirements.txt` içindeki yoruma bakın).

İşkence testleri zor ama basılabilir geometriyi kapsar:

```sh
python3 tests/torture_tests.py
```

### Geliştirici aracı — sentetik defekt enjeksiyonu

`scripts/defect_injector.py` bir geliştirici aracıdır: bir mesh'i bilerek
bozar — seçilen defekt tiplerini (`--hole`, `--non-manifold`, `--self-intersect`,
`--flipped-normal`, `--degenerate`, herhangi bir kombinasyon; `--count N`,
`--seed N`) enjekte eder ve bozuk kopyayı yeni bir dosyaya yazar — girdi asla
değiştirilmez. Onarım hattı ve defekt algılayıcıları için sentetik bozuk
mesh'ler üretmekte ve gelecekteki sentetik eğitim/test setlerini genişletmekte
kullanılabilir. Her enjekte edilen tip, `tests/test_defect_injector.py`
tarafından ilgili algılayıcıya karşı doğrulanır (`defects.detect()` delikler/
non-manifold için, `defect_type_colors()` flipped/dejenere için, pymeshlab
self-intersection self-intersect için).

Bu beş senaryoyu çalıştırır ve her biri için önce/sonra raporlar: bir 5M
üçgenli küre (onarım süresi), 0.05 mm ince levha (özellik kaybı riski — sağlam
kalmalıdır), çok parçalı montaj (8 yüzlü döküntü kaldırma eşiği meşru
parçaları silmemelidir), çok sayıda mikro çatlak içeren kaba bir tarama-tarzı
mesh (kalan delik beklentisi) ve `--mode extreme` ile çalıştırılan iç içe
geçmiş iki küre — ek self-intersection geçişleri kesişen her yüzü kaldırmalı
ve `extreme_passes_applied=True` bildirmelidir.

## Sağlamlık

Bozuk veya düşmanca girdiler, açık bir hata ve sıfır olmayan bir çıkış koduyla
reddedilir; asla çökme veya sessizce yanlış sonuç yoktur:

| Girdi | Davranış |
|---|---|
| Kesilmiş / yarıda kalmış ikili STL | reddedilir: "Unable to open file ... Malformed file" |
| Başlık, dosyanın tuttuğundan fazla üçgen iddia eder | reddedilir: "Malformed file" |
| NaN/Infinity köşe koordinatları | reddedilir: "input mesh contains NaN or infinite coordinates" |
| Boş mesh (0 üçgen) | reddedilir: "input mesh is empty (no triangles)" |
| Tamamen dejenere mesh (yalnızca sıfır alanlı yüzler) | reddedilir: "all faces are degenerate; nothing to repair" |
| Yanlış uzantı (`.stl` içinde OBJ içeriği veya tersi) | reddedilir: "Unable to open file" |

Bunlardan herhangi biri çıkış kodu 1 döndürür, böylece betikler hatayı
güvenilir şekilde algılayabilir.

## Kütüphaneler

| Kütüphane | Rol | Neden |
|---|---|---|
| PyMeshLab | aşama 1 filtre zinciri | VCG tabanlı, baskı onarımı için kanıtlanmış, her boyuttaki deliği doldurur, Python 3.14 wheel'i mevcut |
| manifold3d | aşama 2 katı yeniden kurma | su geçirmezlik garantisi, sağlam boolean, Bambu Studio ile aynı motor |
| trimesh | aşama 2 G/Ç | manifold venv'inde OBJ/mesh yükleme |

`requirements.txt` ve `requirements-311.txt` içinde sabitlenmiştir.

## Bilinen sınırlamalar

* **macOS'ta sağ tık, Dolphin tarzı menü değil Quick Action gerektirir.**
  macOS'ta ServiceMenu karşılığı yoktur; kurulumcu bunun yerine
  `~/Library/Services/` içine bir Finder **Quick Action** ("Sutura — Repair")
  ekler ve seçili STL/3MF dosyaları için sağ tık → Quick Actions altında
  kullanılabilir. Quick Action, bir PyInstaller `Sutura.app` içindeki paketli
  `sutura-cli`'yi çağırır; bu yüzden yalnızca böyle bir uygulama mevcutsa
  (`/Applications` veya `~/Applications` içinde) onarım yapar — dev kurulum
  sarmalayıcı uygulaması tek başına paketli CLI içermez. İndirilmiş (karantinalı)
  bir `Sutura.app`, Quick Action onu çalıştırmadan önce bir kez sağ tık → Aç
  ile açılmalıdır (Gatekeeper; bildirim size bunu söyler).
* **macOS kaldırma betiği yok.** Linux `uninstall.sh`'inin macOS karşılığı
  yoktur; macOS kurulumunu kaldırmak elle yapılır (macOS kurulum bölümündeki
  "macOS kurulumunu kaldırma" bölümüne bakın).
* **Yerel KDE dosya diyaloğu.** GUI, QFileDialog'un yerel KDE diyaloğunu
  (lastik bant dikdörtgen seçimi dahil) kullanması için
  `QT_QPA_PLATFORMTHEME=kde` ayarlar ve `QT_PLUGIN_PATH`'i
  `/usr/lib/qt6/plugins`'e yönlendirir. Bu yalnızca sistem Qt sürümü
  paketlenmiş PySide6 Qt'siyle eşleştiğinde çalışır — GUI, sürümleri
  (`qmake` ile) kontrol eder ve sistem eklentilerini yalnızca eşleşmede
  kullanır. Farklı olduklarında (ör. sistem Qt 6.11.2'ye karşı paketli
  6.11.1), sistem platform eklentileri paketli Qt'ye yüklenemez, bu yüzden
  GUI, Qt'yi kendi paketli eklentilerinde tutar ve gömülü diyaloğa geri
  döner — dikdörtgen seçim kullanılamayabilir, ama Ctrl/Shift+tık her zaman
  çalışır.
* **Bağlı bir kabuk içinde kendisiyle kesişimler.** manifold3d meshi bir katı
  olarak yeniden kurar, bu iç/örtüşen geometriyi çözer, ama patolojik
  durumlarda yeniden kurma, özellikleri hafifçe yeniden şekillendirebilir.
  Sonucu her zaman bir dilimleyicide kontrol edin.
* **Büyük delik yamaları.** VCG, delikleri düz üçgen yamalarla doldurur; çok
  büyük delikler için dolgu basit bir yamadır, akıllı bir yeniden yapılandırma
  değildir. Meshi kapatır ama yama kalitesi ortalamadır ve yumuşatma
  gerektirebilir.
* **Küçük bağlantısız döküntüler.** 8 yüzden az olan bileşenler kaldırılır.
  Ana gövdeye bağlı olmayan küçük, meşru bir parça da kaldırılır.
* **Ters çevrilmiş tüm modeller.** Onarılan hacim negatif çıkarsa tüm mesh
  çevrilir; tutarlı şekilde "içi dışında" sarmalanmış bir model otomatik
  düzeltilir.
* **manifold3d Python bağlaması.** Açık kenarlı herhangi bir girdiyi reddeder;
  aşama 1 bir deliği kapatamazsa aşama 2 atlanır ve aşama 1 sonucu olduğu gibi
  kullanılır (rapor bunu belirtir).
* **Katmanlı/yinelenen köşeli 3MF dışa aktarımları.** Bazı dilimleyiciler
  (Bambu Studio dahil) nesnelerinin her köşe konumunu ~15 kez ayrı köşe girişi
  olarak tekrarlayan ve yüzeyleri katlanmış (bir kenarda birkaç yüz çakışık)
  olan 3MF'ler yazar. VCG bu tür meshleri geçerli 2-manifoldlara dönüştürebilir:
  Aşama 1 zinciri köşe tekilleştirmesinden SONRA yüzleri yeniden tekilleştirir
  (köşe tekilleştirmesi, katmanlı bir mesh üzerinde yinelenen yüzleri aslında
  *oluşturan* adımdır), sonra non-manifold köşeleri delik kapatmadan önce
  onarır ve sonda bir ek kapatma geçişi daha yapar; test dışa aktarımları artık
  tam kapalı onarılıyor — 0 kalan delik (boundary kenarlarının yarısı değil,
  gerçek boundary-loop sayısı olarak raporlanır). Kapalı katmanlı nesneler ayrıca
  nesne başına aşama 2 su geçirmez yeniden kurması alır, bu yüzden
  `stage2_skipped` değil su geçirmez raporlanırlar. Geliştirme sırasındaki bir
  örnek: daha önce 13 ve 26 mikro delik bildiren (veya yinelenen-yüz
  düzeltmesinden önce bazı platformlarda 0 yüze indirilen) 2 nesneli bir Bambu
  dışa aktarımı artık 0 ve 0 bildiriyor, ikisi de aşama 2 doğrulamalı.
* **Tüm nesneler korunur.** Çok nesneli 3MF'ler nesne nesne onarılır ve geri
  yazılır, böylece hiçbir nesne kaybolmaz. Nesne başına sonuç (her nesnenin
  aşama 2 kararı dahil) CLI çıktısında ve GUI'de raporlanır.

## Kullanım geçmişi (anonim, isteğe bağlı kapatılabilir)

Sutura, onarımların *teknik kullanım geçmişini* kaydeder; böylece topluluk,
motoru (mesh_classifier + aşama 1/2 ayarları) gerçekte karşılaştığı vakalara
göre yönlendirebilir. Paylaşılan her kullanım geçmişi motoru zamanla daha
akıllı hale getiriyor — daha fazla gerçek dünya mesh'i, daha iyi classifier
ayarı ve daha az kaçırılan uç durum demek.

* **Ne kaydedilir:** mesh boyutu (köşe/yüz sayıları), kusur sayıları (delik,
  non-manifold, self-intersection), sınıflandırıcı sonucu (tür + güven),
  onarım modu, uygulanan filtre sayısı, nihai kategori, onarım-güven skoru,
  onarım süresi ve yalnızca geometriden türetilen bir parmak izi (tekrar
  eden mesh'leri ayıklamak için).
* **ASLA kaydedilmez:** dosya adları, dosya yolları, kullanıcı adları, IP
  adresleri, makine kimlikleri, ortam değişkenleri veya herhangi bir kişisel
  veri. Parmak izi yalnızca mesh geometrisinden türetilir.
* **Varsayılan:** açık. CLI'da `sutura --no-history ...` ile kapatın veya
  GUI'de ilk-çalıştırma penceresindeki kutuyu işaretlemeyin. Tercih
  `~/.config/sutura/config.json` içinde saklanır (`history_enabled`).
* **Paylaşım:** `sutura export-history` anonim bir özeti ve tam JSON
  kayıtlarını tek komutla basar — bir GitHub issue'suna yapıştırmaya hazır.
  `--summary-only` ile yalnızca özet, `--last N` ile son N kayıt,
  `--clear` ile dosyayı temizleyebilirsiniz. Hiçbir şey otomatik yüklenmez.

## Katkı

Sutura tek kişilik bir emek, ama tek kişilik kalmak zorunda değil. Bug
bulursan issue aç, bir şey eksik/yanlış görüyorsan söyle, mesh onarım
motoruna dokunacak bir fikrin varsa PR gönder. Küçük katkılar (typo
düzeltme, README netleştirme, test case eklemek) de en az büyük özellikler
kadar değerli.

Eksik bir özellik mi var? Onarılmayan bir mesh mi buldunuz? Bir issue açın.
İyi bir hata raporu, kuru bir "çalışmıyor" cümlesinden çok daha değerlidir;
bu yüzden bir mesh bildirirken lütfen şunları ekleyin:

* `sutura <dosya> --human` çıktısı (veya JSON raporu),
* çalıştırdığınız komut,
* ve biliyorsanız meshin nasıl üretildiğini — dilimleyici, tarayıcı, CAD
  dışa aktarımı vb.

Bu, kök nedeni bulmayı çok kolaylaştırır. Yapılandırılmış raporlar önerilir:
`.github/ISSUE_TEMPLATE/bug_report.md`'ye bakın.

## Destekçiler

Sutura bir baskını ya da zamanını kurtardıysa,
[GitHub üzerinden destek olabilirsin](https://github.com/sponsors/Krateian).

(Henüz destekçi yok — ilk sen ol!)

## Lisans

Sutura, v0.2.0'dan itibaren [PolyForm Noncommercial 1.0.0](LICENSE)
ile lisanslanır. Kişisel, ticari olmayan kullanım (hobi, araştırma,
eğitim, kişisel 3D baskı vb.) her zaman ücretsiz kalacak — bu kalıcı
bir taahhüttür. Ticari kullanım için benimle iletişime geçin.

v0.1.0 - v0.1.9 arası yayınlanmış sürümler kalıcı olarak
[Apache 2.0](https://github.com/Krateian/Sutura/blob/v0.1.9/LICENSE)
ile lisanslı kalır.

Bu değişikliğin nedeni: Sutura bir iş planı ya da girişim olarak değil,
kendi ihtiyacımdan doğdu — geliştirirken kullandığım yapay zeka
araçlarının maliyetini kendi cebimden karşılıyorum. Kişisel, ticari
olmayan kullanım için Sutura her zaman ücretsiz kalacak — bu bir
pazarlama sözü değil, kalıcı bir taahhüt. Ama ilerleyen geliştirme daha
fazla zaman ve kaynak gerektirecek, ve bunu bir şirketin ücretsiz
şekilde ticari olarak kullanması bana adil gelmiyor; ben bir hayır
kurumu değilim. Bu yüzden ticari kullanım isteyenlerin benimle
iletişime geçmesi gerekecek. İleride bir noktada projeye zaman
ayıramaz hale gelirsem, Sutura'yı tamamen açık kaynağa çevirip
topluluğa bırakmayı düşünüyorum — ama şimdilik böyle devam.
