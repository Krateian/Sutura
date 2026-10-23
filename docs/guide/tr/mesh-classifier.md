# Mesh türüne duyarlı onarım

[← README'ye dön](../../../README.tr.md) · [English](../mesh-classifier.md)


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

### Opt-in edge-tiebreak kafası (`--experimental-edge-tiebreak`, varsayılan DEĞİL)

Maintainer'ın "ilginç alt-parçaları cımbızla seç" kararı gereği, en güçlü beş
FAZ10 tarama sinyali (`oppmax_std`, `don_mean`, `dihed_mean`, `rmin_mean`,
`gc_mean`) varsayılan vektöre EKLENMEZ (LOO-CV yalnızca ~1 mesh
değiştirdiklerini ve dihedral özelliklerle örtüştüklerini gösterdi — varsayılan
yolda risk alınmaz). Bunun yerine `--experimental-edge-tiebreak` (CLI) veya
GUI'nin **Seçenekler** penceresindeki *Edge-tiebreak classifier* onay kutusuyla
(batch geneli) bir **opt-in 11-özellikli kafa** olarak sunulurlar. 11-özellikli
kafa aynı 71 etiketli mesh üzerinde yeniden eğitildi (LOO-CV 0.845, 5-fold×20
ortalama 0.852'ye karşı temel 0.850). Bu deneyseldir ve varsayılan değildir;
kazanç küçüktür ama altındaki sinyaller istatistiksel olarak gerçektir.
