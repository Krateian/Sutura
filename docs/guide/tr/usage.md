# Kullanım

[← README'ye dön](../../../README.tr.md) · [English](../usage.md)


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

**Otomatik yöntem yedeği.** İlk onarım su geçirmez değilse Sutura birkaç
sıralı yöntem deneyebilir. Denemeden önce, tespit edilen hasarın anlamsız
kıldığı bir yöntemi atlayabilir: kopya yüzler Join, iç yüzler veya bir iç bölge
Transplant ve Wall Thicken yöntemlerini atlar. Bu temel onarımı asla değiştirmez;
`SUTURA_TRIAGE_PRUNE=0` önceki yedek sırasını geri getirir.

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

### Baskı-etkisi raporu

Her onarım raporu, sonucu tek tek koşullara ayıran ve her birinin bir FDM
baskısını nasıl etkilediğini belirten bir `print_report` bloğu taşır. Bu blok
yalnızca zaten hesaplanmış ölçümlerin bir görünümüdür — mesh'i asla yeniden
ölçmez — ve bilinçli olarak rakip bir "iyi mi" hükmü eklemez: tek üst-düzey
yargı yukarıdaki Onarım Sağlığı / Riski / Durumu olarak kalır. Blok dört
parçadan oluşur:

- `fixed` — onarımın gerçekte ne yaptığı, sayılarla: kapatılan delikler,
  düzeltilen non-manifold kenarlar, kaldırılan bozuk yüzler, kaldırılan gevşek
  kabuklar ve yön (winding) düzeltildi mi.
- `issues` — geriye kalan koşullar, her biri bir önem derecesiyle: `breaks`
  (mesh olduğu gibi basılamaz, örn. kalan delikler veya non-manifold kenarlar),
  `may_affect` (örn. birkaç ayrı kabuk, büyük hacim değişimi) veya `cosmetic`
  (kendini-kesişmeler; dilimleyiciler bunları dilim anında tek/çift kuralıyla
  çözer ve basılan parçayı değiştirmez).
- `geometry` — hacim değişimi %, yüzey alanı değişimi % ve şekil sapması
  (`deviation_max_pct` / `deviation_mean_pct`, sonuç yüzeyinin girdiye doğru,
  sınırlayıcı kutu köşegeninin %'si olarak).
- `not_assessed` — bilinçli olarak atlanan ölçümler; bugün 150 000 yüzün
  üzerindeki mesh'lerde şekil sapması (geçit, büyük taramalarda onarımı hızlı
  tutar).

Şekil sapması, onarım zaten bir tane üretmediyse Rust `sutura_geom` nokta-mesh
BVH'si ile ölçülür (Graft'ın vertex-başına sapması veya Dressing'in
`hausdorff_rel_max` değeri varsa yeniden kullanılır). Hacim değişimi için önem
eşikleri (5 % may, 15 % breaks — ikincisi mevcut hacim-uyarı eşiğiyle aynı) ve
sapma için (sınırlayıcı kutu köşegeninin 1 % / 5 %'i) mühendislik tahminleridir,
bir baskı korpusuna göre kalibre edilmemiştir; her sorun `"heuristic": true`
kaydeder ki arayüz bunları tahmin olarak etiketleyebilsin. Çok nesneli 3MF
raporları bloğu nesne başına taşır ve en kötü nesneyi alan bir toplam taşır.

`--human` bloğu bir `Print impact:` bölümü olarak yazdırır (`Fixed:` / `Remaining:` /
`Geometry:` / `Not assessed:`); GUI aynısını "Ne değişti" onarım günlüğünün
başında gösterir.

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

Kaydedilen bir STL, aşama 2 bellekte iki-manifold olsa bile yeniden
yüklendiğinde **sıkışmış (non-manifold) köşeler** taşıyabilir: STL köşe
paylaşımı saklamaz; bellekte ayrı olan iki köşe float32 yazımında aynı olursa
yüklemede birleşir. P-WELD geçişi bu yeniden yükleme biçimini kontrol eder,
neden float32 çakışmasıysa köşeleri ayırır (yüz sayısı değişmez, geometri
sub-ULP düzeyinde oynar) ve `p_weld` alanını
(`non_two_manifold_before`/`after`, `vertices_nudged`, `method`) kaydeder.
Sorun çözülemezse rapor `reload_watertight=false` ve
`stage1.reload_non_manifold_vertices` taşır; dosya su geçirmez değil, uyarıdır.

Çok nesneli 3MF dosyaları yerel olarak işlenir: her nesne meshi bağımsız
onarılır ve arşive geri yazılır, böylece hiçbir nesne kaybolmaz. Aşama 1'in
kapattığı (iki-manifold, kalan delik yok) her nesne, tek-mesh dosyalarıyla aynı
aşama 2 yardımcısından geçer, böylece Manifold su geçirmez yeniden kurmasını
ve nesne başına `stage2` raporunu alır. Otomatik kademelendirme çalıştığında
nesne başına çalışır: temel onarım sonrasında katı-su geçirmez yeniden yüklenen
nesneler el değmeden korunur, başarısız olanlar ise bağımsız olarak sıralanmış
önerilerle onarılır; `object_reports` içindeki her nesne kendi `method_used`
raporunu alır. Rapor sonucu nesne başına listeler
(kalan delik, iki-manifold, aşama 2 kararı) ve `objects_watertight` /
`objects_stage2_ok` özetlerini taşır; dosyanın kategorisi TÜM nesnelerden
türetilir, bu yüzden tek bir hâlâ-açık nesne tüm dosyanın su geçirmez
raporlanmasını engeller. Kusurlar da nesne başına hesaplanır
(`object_reports[i].defects`); 3MF için üst düzey toplu bir `defects` alanı
yoktur. Bayt bayt özdeş geometriye sahip nesneler tek onarımı paylaşır ama her
biri yine kendi nesne başına raporunu alır (aşama 2 sonucu geometrinin saf bir
fonksiyonudur, bu yüzden önbellekteki nesnenin raporu kopyalar için de
geçerlidir).

3MF onarımı tek ve çok nesneli projelerde yıkıcı değildir: Sutura kaynak arşivi
açar ve yalnızca onarılan nesnelerin `<mesh>` XML'ini yeniden yazar. Diğer tüm
girdiler (küçük resimler, dilimleyici ayarları, plaka ve wipe-tower verileri,
içerik türleri, ilişkiler ve `3D/Objects/` altındaki Production uzantılı alt
modeller) adlarını, sırasını, açılmış baytlarını, sıkıştırma türünü ve ZIP meta
verilerini korur. Nesne/build kimlikleri, bileşen dönüşümleri, birimler ve ad
alanı bildirimleri korunur. Aşama 1 ayrıca köşe koordinatları değişmeyen
üçgenlerdeki üçgen başına dilimleyici özniteliklerini (`paint_color`,
`paint_seam`, `paint_supports`, `paint_fuzzy_skin`, `slic3rpe:mmu_segmentation`,
`pid`/`p1`, ...) korur; yeni oluşturulan üçgenlere boya aktarılmaz. JSON raporu
nesne başına `triangle_attributes` (`original_attributed`, `kept`, `dropped`,
`output_triangles`) ve çok nesneli 3MF için toplam bir dosya düzeyi değeri
taşır. `tests/test_3mf_roundtrip.py` ile regresyon testi yapılır.

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
temasından bağımsız olarak her platformda ve Qt sürümünde aynı görünür. Batch
geneli onarım seçenekleri — fTetWild yedek katmanı ve isteğe bağlı deneysel
adımlar — ayrı bir **Seçenekler** penceresindedir (**Mod** düğmesinin
yanındaki düğme ya da Ctrl+, / Cmd+,); onarım sürerken açık kalabilir, düğme
etiketi varsayılandan farklı seçenek sayısını gösterir, örneğin *Seçenekler
(2)*. Sekmeleri: *Genel* (anonim kullanım geçmişi), *Onarım* (fTetWild yedek
katmanı ve Triage yoğunluk preset/profil seçicisi ile profil düzenleyicisi),
*Deneysel* (isteğe bağlı adımlar), *Güncellemeler* (elle kontrol, otomatik
güncelleme, isteğe bağlı her açılışta kontrol), *Değişiklikler* (GitHub'dan
sürüm notları, çevrimdışı yedekli) ve *Motorlar* (fTetWild kur/kaldır ve
yapılandırılmış harici motor listesi).

#### Onarım öncesi analiz

![Onarım öncesi analiz](../../../assets/analyze-panel.png)

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

![Kusur detay paneli](../../../assets/defect-panel.png)

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
kusurlarını kırmızıyla `(235,60,70)` işaretler. Onarılmış görünüm üç durumlu bir renk
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

![Öncesi/sonrası karşılaştırma diyaloğu](../../../assets/before-after-panel.png)

Diyalogun üstündeki **Statik / İnteraktif** anahtarı, etkileşimli 3D
görünümü ekler (ilk kez seçildiğinde tembel üretilir, sonra diyaloğun ömrü
boyunca önbellekte tutulur): **sürükleyerek** mesh döndürülür, **fare
tekerleğiyle** yakınlaştırılır. Sürükleme sırasında düşük-poligonlu bir LOD
arka plan iş parçacığında her karede çizilir (en yeni kare kazanır, GUI
sürecinde mesh yükleme yoktur); durduktan ~300 ms sonra tam çözünürlüklü bir
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

Ayrıca `orcaslicer-plugin/` altında, **yeni OrcaSlicer betik-eklentisi
API'si** için **deneysel** bir [OrcaSlicer betik eklentisi](../../../orcaslicer-plugin/)
vardır. OrcaSlicer **2.5.0+ / nightly sürümlerini** hedefler — 2.4.x sürümlerinde
"Plugins" menüsü yoktur, bu yüzden eklenti orada çalışmaz.

Eklentiyi çalıştırmak, 3D görünümün yanında (ve devre dışı bırakılmadıkça
başlangıçta otomatik olarak) yerleştirilebilir bir **"Sutura" paneli** açar.
Panel, **plate'teki her nesneyi** listeler ve bir Quick / Balanced / Thorough /
Extreme preset seçici (varsayılan Balanced), nesne başına **Analyze**
(salt-okunur: delikler, non-manifold bölgeler, self-intersection'lar ve en üst
sıradaki onarım yöntemleri) ve **Repair**, bir "Repair selected (n)" alt bilgi
düğmesi ve bir "Select broken" kısayolu, Cancel düğmeli canlı iş aşamaları
(queued → exporting → repairing → loading → done / failed / cancelled) ve
tamamlanan bir iş için bir **Show file** bağlantısı sunar. Repair ve Analyze
her zaman kurulu Sutura CLI'sı üzerinden ayrı bir süreç olarak çalışır.

OrcaSlicer'ın içe aktarımda onardığı nesneler (mesh hataları) veya parçaları
non-manifold olan nesneler, **nesne başına oturumda bir kez** "Repair with
Sutura" eylemli bir uyarı bildirimi üretir. Dışa aktarım, bir nesnenin her
model-parça hacmini **dünya koordinatlarında** yazar (aynalanmış parçalar dışa
bakan sarmalımı korur), parametre değiştiricilerini, negatif hacimleri ve
destek engelleyicilerini atlar ve çok parçalı bir nesneyi tek bir STL'de
birleştirir. Onarılan kopya **yeni bir nesne** olarak geri yüklenir; dışa
aktarım dünya koordinatlarında olduğundan OrcaSlicer onu orijinalin üzerine
yerleştirebilir — ayırmak için **A** (Arrange), kaldırmak için **Ctrl/Cmd+Z**
tuşlarına basın. Çıktı dosyaları, eşzamansız geri yükleme tamamlanırken
OrcaSlicer okuyabilsin diye yedi gün saklanır. `numpy` beyan edilmiş bir eklenti
bağımlılığıdır (OrcaSlicer'ın paketli uv'si kurar); numpy'siz bir geri dönüş
yolu da vardır.

Kurulum (OrcaSlicer 2.5.0+ / nightly):

1. Önce Sutura'yı kurun (Linux'ta `./install.sh`, macOS'ta `install-macos.sh`)
   böylece `~/.local/bin/sutura` var olur.
2. Eklentiyi OrcaSlicer'ın eklenti dizinine kopyalayın. Linux:

   ```sh
   mkdir -p ~/.config/OrcaSlicer/orca_plugins/SuturaRepair
   cp sutura_repair_linux_x86_64.py ~/.config/OrcaSlicer/orca_plugins/SuturaRepair/
   ```

   macOS: üst dizin olarak `~/Library/Application Support/OrcaSlicer/orca_plugins/`
   kullanın. Dosya adı bir adlandırma artığıdır — aynı tek dosya hem Linux'ta
   hem macOS'ta çalışır.
3. Eklentiyi OrcaSlicer **Plugins** diyaloğunda etkinleştirin, ardından paneli
   açmak için çalıştırın.

İzinler: `register_capabilities()` var olan Sutura CLI yolunu
`request_permissions(fs_read=...)` ile, ayrıca `threading` ile ve önceden
adlandırabildiği iki kararlı süreç hedefiyle (OrcaSlicer süreç sayımı yoklaması
ve sabit klasör gösterme komutu) önceden beyan eder. Bu anahtar sözcükleri kabul
eden bir sürüm yüklemede yalnızca bir kez istem gösterir. Gönderilen
**2.5.0-alpha** yalnızca `fs_read` kabul eder; bu yüzden eklenti fs_read-yalnız
beyana geri düşer (okuma izni korunur) ve kullanıcı **yüklemede bir dosya okuma
istemi** ile **ilk kullanımda bir kez, tetiklendiğinde "bir iş parçacığı
oluştur" istemi** ile karşılaşır; onaylandığında izin kalıcı olur ve sonraki
Repair, Analyze ve Show file çalıştırmaları başka istem göstermez. Repair ve
Analyze başlatmaları eklentinin oluşturduğu çalışan iş parçacıklarında
gerçekleşir ve bu iş parçacıklarının denetim bağlamı boştur, dolayısıyla 2.5'te
denetlenmezler. Bir ret, panelde başarısız olan işlemi adlandırarak bildirilir —
kırmızı banner yalnızca repair veya Analyze işi başlatılamadığında, arka plan
yenilemesi veya taraması reddedildiğinde ise sakin bir uyarı gösterilir. Gerçek
bir OrcaSlicer **2.5.0-dev** nightly (macOS) üzerinde uçtan uca doğrulandı;
eklenti API'sini içeren ilk stabil sürüm 2.5.0'dır; birincil hedef
Linux'tur ve aynı dosya doğrulanmış bir bonus olarak macOS'ta da çalışır.
Tam ayrıntı ve sınırlamalar için [eklenti README'sine](../../../orcaslicer-plugin/README.md)
bakın.
