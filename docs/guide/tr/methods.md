# Onarım yöntemleri, presetler ve motorlar

[← README'ye dön](../../../README.tr.md) · [English](../methods.md)

## Triage Engine yoğunluk presetleri ve adlandırılmış profiller

Aşama 1'den *sonra* harcanan çaba — derin-onarım merdiveni, fTetWild
katmanının açık/kapalı durumu, girdi boyut sınırı ve duvar-saati bütçesi,
Graft shell-wrap katmanının kendi girdi boyut sınırı ve duvar-saati bütçesi,
yoğun sınır sadeleştirme merdiveni ve Hausdorff örnek sayısı — dört salt-
okunur preset ve istenildiği kadar kullanıcı profili ile toplanmıştır:
`--intensity <ad>` (veya `SUTURA_INTENSITY`, veya
`~/.config/sutura/config.json` içindeki `intensity` anahtarı; bayrak, sonra
ortam, sonra config kazanır). Aşama 1 bilinçli olarak **etkilenmez**: onarım
modu (`--mode`/sınıflandırıcı) ve isteğe bağlı profil (`--profile`) her
preset'te önceki davranışını korur. Seçilen yoğunluk raporda
`triage_intensity` olarak ve `--human` çıktısında bir `Intensity:` satırı
olarak görünür; `--list-intensities` presetleri ve profilleri etkin
değerleriyle yazdırıp çıkar.

- **Quick** — derin-onarım merdivenini ve fTetWild katmanını tamamen atlar
  (`deep_repair=off`, fTetWild kapalı) ve sadeleştirme merdiveni boştur: en
  hızlı çalışma; zor ağları açık bırakır.
- **Balanced** — varsayılan ve mevcut sürüm davranışı: her düğme tarihsel
  `repair.py` sabitidir (kuruluysa fTetWild `auto`, 180 sn bütçe, 300.000
  yüz sınırı, Graft 300 sn bütçe ve 2.000.000 yüz sınırı, `(1.5, 3.0)`
  sadeleştirme merdiveni, 200.000 Hausdorff örneği). `--intensity balanced`,
  bayraksız çalıştırmayla bayt-birebirdir.
- **Thorough** — 600 sn fTetWild bütçesi, 900 sn Graft bütçesi, 400.000
  Hausdorff örneği ve bir fazladan sadeleştirme basamağı, `threshold`
  (= `DENSE_RATIO × max(girdi, 20.000)`).
- **Extreme** — fTetWild ve Graft için girdi boyut sınırı yok, ikisi için
  1.800 sn bütçe, 1.000.000 Hausdorff örneği, fazladan `threshold` basamağı
  ve savurgan yoğunlukta bir sınırın
  her basamağı başarısız olduğunda, sadeleştirilmemiş sınıra dönmeden önce
  tetrahedron kalite optimizasyonu açık bir fTetWild çalışması daha.

Fazladan `threshold` basamağı, savurgan yoğunlukta bir fTetWild sınırının
alt `1.5×`/`3×` hedeflerinde su geçirmez sadeleştirilemediği durumu hedefler.
thingi10k_73444'te `optimize=False` sınırı 469.144 yüz döndürdü; `20.000` ve
`23.646` hedefleri su geçirmezlik kontrolünü geçemedi (`not_watertight`),
80.000'lik `threshold` hedefi ise başarılı oldu ve su geçirmez sonuç verdi
(uygulandı, `shape_changed` %5 olarak işaretlendi — şekil koruması
değişmezdir ve hiçbir preset'in düğmesi değildir). fTetWild ve Hausdorff
*korumasının* kendisi (su geçirmez + `FTETWILD_MAX_HAUSDORFF_REL`) bir düğme
değildir: her preset'te ve her profilde açık kalır.

**Adlandırılmış profiller** presetlerin üzerine oturur. Her profil bir taban
preset artı seyrek alan geçersiz kılmalarıdır (yalnızca değiştirilen alanlar
saklanır); dört yerleşik preset salt-okunur kalır. Profiller
`~/.config/sutura/profiles.json` içinde saklanır — ayrı, şema-sürümlü ve
atomik yazılan (geçici dosya + rename) bir dosyadır; `config.json` anahtarı
değildir, böylece bozuk bir profil dosyası yapılandırmanın geri kalanını
bozamaz. Bozuk, okunamayan, yanlış şemalı veya eksik dosya stderr'e uyarı
yazıp yerleşik presetlere döner; hiçbir zaman onarımı çökertmez. Adlar boş
olamaz, büyük/küçük harf duyarsız benzersiz olmalı ve yerleşik bir preset
adını gölgeleyemez. Profil tıpkı bir preset gibi seçilir
(`--intensity benim-profilim`); bir profil kullanıldığında rapor
`triage_intensity` yanına `triage_profile_base` ve `triage_overrides` ekler.
Hausdorff örnek alt sınırı (200.000) çözücüde de uygulanır: elle düzenlenmiş
bir profil bunun altındaysa uyarıyla sınırlanır. GUI'de (Seçenekler →
*Onarım*) açılır liste önce presetleri, bir ayırıcı, sonra kullanıcı
profillerini gösterir; *Yeni*, *Çoğalt*, *Yeniden adlandır* ve *Sil* (onaylı)
düğmeleri ile her alan için satır içi *Profil ayarları* düzenleyicisi vardır;
yerleşik bir preset düzenlenirse *Yeni profil olarak kaydet* sunulur ve
*Önerilene sıfırla* hiçbir profili silmeden Dengeli'yi seçer.

Orijinal dosya asla üzerine yazılmaz. Çıktı aynı dizinde `_fixed` sonekiyle
yazılır.

## Onarım yöntem kayıt defteri ve nesne-başına öneriler

`sutura_engine.methods` (`sutura/methods.py` üzerinden geriye dönük uyumlu),
on dokuz onarım **yönteminin** genişletilebilir kayıt defteridir (`--list-methods`
numara, kimlik, aile, kullanılabilirlik ve görünen adı yazdırır). 1–7 numaralı
yöntemler temel boru hattı üzerinde seçicilerdir (#1 Hızlı Temizlik, #2 Yerel Onarım,
#3 Tam Onarım, #4 Birleştir, #5 Kendini Onarım, #6 Kesin Onarım, #7 fTetWild);
#8–#10 bağımsız yüzey yeniden inşa katmanlarıdır (#8 Balon native örtük kapatma,
#9 Arka Plaka düz kapatma, #10 İskelet vekil şablon onarımı); #11 Nakil ve #12 Nakil+
otomatik ve nokta kılavuzlu yineleme onarımlarıdır; #13 Graft sağlıklı üçgenleri
birebir koruyarak hasarı kapatan morfoloji kabuk sarma katmanıdır; #14 Aynalı Tamamlama
simetrik taramaların eksik arkasını tamamlar; #15 Duvar Kalınlaştır isteğe bağlı
morfolojik ince-duvar kalınlaştırıcıdır (asla otomatik çalışmaz); #16 Pansuman
isteğe bağlı, değişken viskoziteli seviye kümesi kaplamasıdır (aşağıda kendi bölümü);
#17 Tamponad, açık sınırları içeriden dolduran ve geri alınan katmanlardaki
kalan kendini kesmeleri eriten varsayılan açık iç tıkaçtır (takma ad `lava`,
aşağıda kendi bölümü); #18 Splint isteğe bağlı tam katı yeniden inşa katmanıdır
(varsayılan kapalı); ve #19 Koaptasyon, float32-ULP dikiş kaynağı + kesin
birleştirme katmanıdır (varsayılan açık, yalnızca kesinlikle daha iyiyse
benimsenir; aşağıda kendi bölümü).
8–10 numaralı yöntemler girdi üzerinde çalışır (fTetWild katmanı gibi) ve
sonuçlarını yalnızca katı-su geçirmez olduğunda ve özgün yüzey hâlâ
kapsandığında (tek-taraflı girdi→çıktı Hausdorff koruması) benimser, böylece
tahmin edilen arka yüzey modeli sessizce kaydıramaz; modülün notları (örneğin
"back surface was estimated") üst düzey `closing` rapor anahtarında görünür.
11–12 numaralı yöntemler stage-1 zincirinden sonra su geçirmez sonuç üzerinde
çalışır: #11 döner/ötelemeli/helisel bir yineleme deseni bulup hasarlı veya
eksik her ögeye sağlam bir kopyayı nakleder, #12 kullanıcının
`--repeat-source X,Y,Z` noktasına en yakın ögeyi `--repeat-target X,Y,Z`
noktasına en yakın ögenin üzerine nakleder. Nakil yalnızca su geçirmez
kaldığında, temelden daha kötü olmadığında ve dokunulmamış geometri
kaymadığında (`hausdorff_outside`) benimsenir; `repeat` rapor anahtarı desen
tipini ve onarılan konum sayısını taşır.

### Onarım yöntemleri (#1–#19)

`--list-methods` çıktısındaki kararlı tamsayı sırasıyla on dokuz yöntem (kanonik
kimlikler gösterilmektedir; eski slug'lar takma ad olarak desteklenir):

| # | Kanonik ID | Görünen Ad | Aile | Ne yapar | Geometri uydurur mu? |
|---|---|---|---|---|---|
| 1 | `quick_clean` | Hızlı Temizlik | temizlik | Aşama 1 + Manifold yeniden inşası; derin onarım ve fTetWild yok. | hayır |
| 2 | `local_mend` | Yerel Onarım | topoloji | Derin onarım merdiveni `local`: yalnızca hasarlı bölgeleri yeniden mesh'ler. | hayır |
| 3 | `full_mend` | Tam Onarım | topoloji | Derin onarım merdiveni `full` (varsayılan dengeli davranış). | hayır |
| 4 | `join` | Birleştir | topoloji | Küçük ayrık bileşenleri silmek yerine en yakın büyük bileşene taşır. | hayır (taşır) |
| 5 | `autorefine` | Kendini Onarım | si | Kesişen üçgenleri kesişim segmentleri boyunca böler (Lazard & Valque 2025). | hayır |
| 6 | `exact_refine` | Kesin Onarım | si | Rust çekirdeği (`sutura_geom`) ile tam düzenleme-lite kendini kesme ayırması. | hayır |
| 7 | `ftetwild` | fTetWild | zarf | Özgün yüzeyi tetrahedralize edip su geçirmez, SI'sız bir sınır çıkarır. | evet (hacimsel ağ) |
| 8 | `balloon` | Balon | kapama | Tek-taraflı taramaların büyük açıklıklarını native örtük (genelleştirilmiş sarım seviye kümesi) yöntemle kapatır. | evet (arka yüzey) |
| 9 | `backplate` | Arka Plaka | kapama | Kabartma/plakalar için düz arka düzlem ve yan duvarlar; ön yüzü korur. | evet (arka düzlem) |
| 10 | `scaffold` | İskelet | şablon | Kaba su geçirmez vekil gövde ve sağlıklı özgün bölgelerin yeniden izdüşümü. | evet (vekil) |
| 11 | `transplant` | Nakil | desen | Yineleme desenini bulur ve hasarlı/eksik parçalara sağlam kopyayı nakleder. | evet (nakil) |
| 12 | `transplant_plus` | Nakil+ | desen | Kullanıcının işaretlediği ögeler arasında etkileşimli nakil; isteğe bağlı. | evet (nakil) |
| 13 | `graft` | Graft | zarf | Sağlıklı üçgenleri birebir koruyarak hasarı kapatan morfoloji kabuk sarma. | evet (kabuk sarma) |
| 14 | `mirror_complete` | Aynalı Tamamlama | kapama | Simetrik tek-taraflı taramayı tespit edilen simetri düzleminden aynalar. | evet (aynalı arka) |
| 15 | `wall_thicken` | Duvar Kalınlaştır | zarf | SDF ızgarasından ince duvarları tespit edip morfolojik öteleme ile kalınlaştırır. | evet (öteleme) |
| 16 | `dressing` | Pansuman | zarf | İsteğe bağlı, değişken viskoziteli seviye kümesi kaplaması; yapısı gereği 2-manifold ve SI'sız. | evet (viskozite kaplama) |
| 17 | `tamponade` | Tamponad | iç | Varsayılan açık iç tıkaç (takma ad `lava`): her açık sınırı içeriden doldurur, geri alınan katmanlardaki kalan kendini kesmeleri eritir; **Stent** adayı tıkacı kenardan içe doğru şişirir (büyük halkalar için çok-stent bölümlemesi); hiçbir girdi yüzünü silmez. | evet (tıkaç) |
| 18 | `splint` | Splint | katı | İsteğe bağlı kesin katı yeniden inşa: proxy yüzey + Delaunay dörtyüzlüleme, iç kısmın kesilip atılması (Rust çekirdeği); deterministik, eklenti gerektirmez. | evet (katı yeniden inşa) |
| 19 | `coaptation` | Koaptasyon | katı | Varsayılan açık: katlanmış/üst üste binen çok-gövdeli bir çorbanın bir-float32-ULP'lik dikişini kapatır (yalnızca-sınır tolerans kaynağı) ve kapatılan gövdeleri kesin birleştirme ile katlar; yalnızca merdiven sonucundan kesinlikle daha iyiyse benimsenir. | evet (birleştirme katlama) |

Geometri uyduran yöntemler asla korumasız çalışmaz: #7 tek-taraflı çıktı→girdi
Hausdorff korumasıyla; #8, #9, #10 ve #14 girdi→çıktı korumasıyla (özgün yüzey
tamamen kapsanmalıdır); #11 ve #12 dokunulmamış geometrinin kaymasını engelleyen
`hausdorff_outside` ile; #13 Graft sağlıklı ayrıntı kaybında otomatik olarak
fTetWild'a dönen kendi özgün→çıktı sadakat koruması (`fidelity_ok`) ile; #15
Duvar Kalınlaştır girdi→çıktı korumasıyla sınırlandırılmıştır (#15 şekil kararı
olduğu için sıralamaya dahil edilmez, açıkça seçilmelidir); #16 Pansuman ise
kendi kaplama→girdi sadakat kapısıyla korunur ve aynı şekilde sıralamaya dahil
edilmez (korpus üzerinde ölçülene kadar isteğe bağlıdır); #17 Tamponad ise kendi
yeniden-yükleme-dürüst delik/non-manifold/parça-sayısı/sağlıklı-yüz denetimiyle
korunur; boru hattında varsayılan olarak açıktır ama açık bir alternatif olarak
sıralamaya dahil edilmez. #19 Koaptasyon kendi sıkı benimseme denetimiyle
korunur (sıfır kesin kendini kesme ve daha kötü olmayan delik/non-manifold
kenar/bileşen sayısıyla su geçirmez bir birleştirme); her tam-mod girdisinde
çalışır ve iyileştiremediğinde merdiven sonucunu bayt düzeyinde aynı döndürür.

### Nesne analizi ve öneriler

`--analyze <dosya>` ucuz bir nesne-başına analiz yapar (yüz/tepe sayısı,
bileşenler, sınır döngüleri, açık-alan oranı, tek-taraflı/kabartma ve
yineleme puanları, manifold olmayan kenarlar, kendini kesen yüzler, mesh tipi)
ve sıralanmış
yöntem önerilerini yazdırır;
üçüncü taraf motorlar **ayrı** bir bölümde listelenir ve sıralamaya asla
karıştırılmaz. Kendini-kesme ölçümü 200.000 yüzle sınırlıdır — bunun üzerinde
örnekleme yapılır ve değer tahmin olarak işaretlenir. `--methods 2,3,5`
denenecek yöntemleri sırayla etiketler. Bayrak verilmezse onarım **otomatik**tir:
önce bugünkü varsayılan boru hattı çalışır ve sonucu zaten katı-su geçirmezse
çıktı öncekiyle bayt-birebirdir. Yalnızca bu temel başarısız olursa sıralanmış
yöntemler denenir (en fazla üç ek deneme; fTetWild gibi geometri uyduran bir
yöntem yalnızca öneri puanı yeterince yüksekse), `min(max(30 sn, 2 × temel),
120 sn)` duvar-saati bütçesi altında: bütçe tükenince başka yöntem başlatılmaz
(çalışan bir deneme kesilmez), en iyi aday korunur ve sonuç "yedek yöntem
bütçesine ulaşıldı" notuyla işaretlenir. Temel onarım tek başına 120 sn'yi
aştıysa sıralanmış yedek yöntemler tamamen atlanır ("yedek yöntem atlandı:
temel onarım çok yavaş"). Açık bir
`--deep-repair`/`--no-fallback-ftetwild` bugünkü davranışı korur, ek denemeye
geçmez; açıkça etiketlenen yöntemler (`--methods`) hiçbir zaman bütçelenmez.
Çok nesneli 3MF dosyalarında otomatik kademelendirme nesne başına çalışır:
temel onarımda su geçirmez olan nesneler el değmeden korunur, başarısız olanlar
bağımsız olarak önerilen yedek yöntemlerle onarılır (nesne başına `method_used`
raporlanır). Rapor `method_used`, `methods_tried`, `method_reached_watertight` ve
ek deneme çalıştıysa `analysis` ile `recommendations` alanlarını ekler.

**Öğrenen triyaj (yerel, sınırlı).** Anonim kullanım geçmişi etkinse her onarım
aynı zamanda `(nesne tipi, yöntem)` başına sonucu (kabul/red) ve geçen süreyi
`~/.local/share/sutura/triage_learning.json` dosyasına kaydeder (yalnızca şablon
kimlikleri ve yöntem slug'ları — geçmişle aynı gizlilik kuralı). `rank_methods`
ardından küçük bir Bayes bonusu ekler: Beta(2, 2) posterior başarı ortalaması
`[0, 0.05]` aralığına ölçeklenir, hız çarpanıyla azaltılır ve veri yoksa tam
olarak sıfırdır. Bonus, geometrik analizin zaten öne çıkarmadığı bir yöntemi asla
yükseltmez. `learning_triage` yapılandırma anahtarı / `SUTURA_LEARNING_TRIAGE` /
`--no-learning-triage` ile açılıp kapanır ve kullanım geçmişine dokunmadan
CLI'den (`sutura clear-learning`) veya GUI **Seçenekler → Genel → Öğrenmeyi
sıfırla** düğmesinden sıfırlanır. Ağ erişimi kullanılmaz.

### GUI'de dosya-başına yöntem etiketleme (sağ tık menüsü)

Aynı kayıt defteri GUI'de de açığa çıkar. Dosya listesinde bir **Yöntem**
sütunu *Otomatik*, analizden sonra *Otomatik (öneri #3 … %82)* ya da
kullanıcının seçtiği etiket zincirini gösterir. Bir veya birkaç dosyaya sağ
tıklamak bir menü açar: *Analiz et* nesne-başına analizi (`--analyze`) arka
planda çalıştırır, *Önerilen yöntemler* sıralanmış en iyi yöntemleri bir puan
çubuğu ve gerekçesi ipucunda olacak şekilde listeler, *Yöntem kullan* on dokuz
yöntemin (#1–#19) tümünü sırayı koruyan işaretlenebilir bir açılır menü olarak
listeler (işaretlenme sırası deneme sırasıdır, örn. `#2 → #3 → #5`); #12'yi
seçmek, nesneyi CPU ile tarayan (OpenGL yok) küçük bir seçici açar; kullanıcı
önce sağlam kaynak ögeye, sonra hasarlı hedef ögeye tıklar ve *Harici
motorlar* yapılandırılmış üçüncü taraf motorları listeler (sıralamadan ayrı
tutulur). *Etiketleri temizle* bir dosyayı Otomatik'e döndürür. Etiketli
dosyalar tam olarak kendi yöntemlerini sırayla çalıştırır; etiketsiz dosyalar
otomatik davranışı korur. Dosya başına etiketlenen motorlar CLI'de
`--engines AD[,AD]` ile seçilir (`None` yapılandırılmış tüm etkin motorları
çalıştırır).

![Dosya listesi sağ tık yöntem menüsü](../../../assets/method-menu.png)

### Yinelenen öge onarımı (otomatik/elle seçici)

`--methods 11` (otomatik) ve `--methods 12` (elle), bir eşleşik özelliği
yinelenen modelleri hedefler — dişli dişleri, tırtıklar, vida sırtları, düzenli
bir karo/çıkıntı ızgarası, aynı parçaların 3B kafesi — ve bunlardan birinin ya
da birkaçının kırık, çökük veya dışa taşmış olduğu durumları. Algılama
**bölütlemesizdir**: yüzeyi örnekler, temel eylemsizlik eksenlerini kurar ve
döner mertebeleri 2–64 (artı yüksek tırtık sayıları 72/80/90/100/120) ile
ötelemeli adımları — 2B'de ve tam 3B kafeste — çok-adımlı bir doğrulamayla
sınar; bu doğrulama, temel-mertebe yanılsamasını (24 dişli dişlide `n=8` ile
`n=24`) reddeder. Hizalama, katı dönüşümlerin yanı sıra tekdüze **benzerlik**
(ölçek) ve **ayna**yı destekler. Hasarlı öge, hacimsel bir Manifold CSG
nakliyle sağlam bir tanesiyle değiştirilir; eksik malzeme ileri nokta-yüzey
sapmasıyla, dışa taşan bir çıkıntı ters mesafeyle bulunur ve oyuk kesimi
%0,1 küçültülür, böylece verici ile oyuk hafifçe üst üste biner (aksi halde
float32 STL yazımında manifold olmayan kenarlara çöken eş düzlemli sınır
ortadan kalkar). Çok-bileşenli montajlarda `extract_outer_shell`, parçaları
göreli bir epsilon ile genişletip boolean birleşimle tek bir dış kabuğa indirir;
onarımın kendisi su geçirmez kaldığında, temelden daha kötü olmadığında ve
dokunulmamış geometri kaymadığında (`hausdorff_outside`) benimsenir.

Sahibin `patterned samples` setinde (#11 otomatik yolu):

| model | yüz | yineleme puanı | desen | hasarlı öge | dış Hausdorff | yeniden yükleme kararı |
|---|---|---|---|---|---|---|
| `gear 24teeth 2x.stl` | 30.144 | 0,649 | döner C₂₄ | diş #18'de 1,5 mm çökme | 0,000000 mm | su geçirmez (0 delik, 0 NM, cins 1) |
| `PET Bottle Cap.stl` | 29.792 | 0,669 | döner C₁₂₀ | 0,51 mm tırtık çökmesi | 0,0849 mm (ortalama 0,00006) | su geçirmez (0 delik, 0 NM) |
| `rubics-cubestl.stl` | 118.956 → 64.380 (dış kabuk) | 0,616 | 3B kafes (3×3×3) | köşe küpünde 3,5 mm çökme | 0,000000 mm | su geçirmez (0 delik, 0 NM, cins 0) |

**Dürüst sonuçsuz örnekler.** Algılanan her simetri onarılabilir değildir ve
katman, zorla nakil yapmak yerine gerekçeyi bildirir. Antep fıstığı kasesinde
(369.732 yüz, puan 0,749) ve Deniz Kabuğu Vazosu'nda (359.910 yüz, puan 0,763)
algılanan mertebe ayrık bir eşleşik dizi değil, *sürekli* bir sarmal kabuk /
ölçeklenen bir kaburga yelpazesidir; katı bir CSG nakli 1,5 mm'lik kabuğu
yırtacağından `positions_repaired` 0'dır. `large-shell.stl`'de (1.318.202 yüz)
puan 0,000'dır — değişken boyutlu büyümeyle gerçek organik morfoloji, eşleşik
yineleme yok. Üçü de değiştirilmeden (stage-1 çıktısı) yazılır, asla sessizce
yeniden şekillendirilmez.

### Tek-taraflı tarama kapatma

`sutura/closing.py` içindeki #8 (`poisson_close`) ve #9 (`flat_back_close`),
baskın bir açıklığı olan **tek-taraflı** bir tarama veya kabartmayı ele alır —
"arka" eksiktir, dolayısıyla hiçbir onarım gerçek geometriye karşı
doğrulanamaz. #8 açık sınır üzerinde native genelleştirilmiş-sarım işaretli
seviye kümesini çıkarır; #9 ölçülen yüzeyi tam olarak korur ve açıklığı düz bir arka düzlem ile
yan duvarlarla kapatır (kapak sınır-döngüsü yönünü izler, böylece saat yönü
döngüler artık ters kapak üretmez). İkisi de **özgün** girdi üzerinde çalışır,
temizlenmiş diziler üzerinde değil (stage 1 döngüyü zaten düz-kapatmış olurdu)
ve sonuç yalnızca katı-su geçirmez olduğunda ve özgün yüzey tek-taraflı bir
girdi→çıktı Hausdorff korumasıyla hâlâ kapsandığında benimsenir. Modülün
notları (örn. "back surface was estimated") `closing` rapor anahtarına düşer.

### Aynalı tamamlama

#14 (`mirror_complete`, `sutura/mirror_repair.py`) simetri-farkındalı, bir
tek-taraflı taramada örtük Balloon kapatmaya alternatiftir. `detect_mirror_plane` en
baskın sınır döngüsünün düzlemini uydurur ve bu döngünün ne kadar düzlemsel ve
baskın olduğunu puanlar; güven eşiği aşarsa `mirror_close` görünen yüzeyi bu
düzlemde yansıtır (sarım ters çevrilir, böylece paylaşılan sınır kenarları
birbirini götürür ve dikiş kapanır), dikişi kaynaklar ve çakışan yarım küreleri
`extract_outer_shell` ile tek bir dış kabuğa indirir. Simetri güveni düşükse ya
da aynalı dikiş geçerli bir katı değilse native örtük kapatmaya (#8)
geri döner; `closing` raporundaki `mode` alanı hangi yolun çalıştığını belirtir
(`mirror` / `poisson_fallback`) ve `mirror_score` güveni kaydeder.

### İnce duvar analizi ve kalınlaştırma

`sutura/wall_thickness.py` (#15 `wall_thicken` yönteminin arkasında) bir mesh'in
yerel duvar kalınlığını işaretli-mesafe-alanı ızgarasından tahmin eder: alan
ağırlıklı yüzey örnekleri ve en yakın-normal işareti, ardından her yüz merkezinden
içe doğru ilk sıfır geçişine ışın yürüyüşü (en yakın ters-bakan örnek yedeğiyle).
`estimate_wall_thickness` **ısı-haritasına hazır** tepe-başına bir dizi ile
`min`/`median`/`mean` ve hedefin altındaki tepe sayısını döndürür.
`thicken_to_min` ince duvarları katının morfolojik genişlemesiyle öteler
(`delta = (hedef - min) / 2`, bbox köşegeninin %25'i ile sınırlı) ve yüzeyi
bağımlılıksız bir marching-tetrahedra adımıyla yeniden çıkarır. #15 tasarım
gereği isteğe bağlıdır (`needs_user_input`, sıralamadan çıkarılır ve asla
otomatik kademelendirilmez) ve açıkça seçilir — `--methods 15`, isteğe bağlı
`--wall-min-thickness T` (varsayılan bbox köşegeninin %1'i).

### Vekil şablon onarımı

#10 (`proxy_template`, `sutura/proxy_repair.py`), ağır hasarlı ama başka yönden
sağlıklı bir mesh'i hedefler: native örtük (genelleştirilmiş sarım seviye kümesi) yeniden inşasından kaba bir su
geçirmez vekil kurar, ardından **sağlıklı özgün bölgeleri** vekile yeniden
izdüşürür; böylece ayrıntılı yüzey korunur ve yalnızca eksik parçalar vekilden
gelir. Bir yüz bütçesiyle sınırlıdır ve kapama katmanları gibi yalnızca
katı-su geçirmez olduğunda ve özgün yüzey kapsanmış kaldığında (girdi→çıktı
koruması) benimsenir.

### Flap (yüzey delik doldurma)

Flap (`sutura_engine.flap`), bir sınır halkasını üzerine düz bir kapak
oturtmak yerine komşu yüzeyi sürdürerek kapatan yüzey tabanlı bir delik
doldurucudur. Ham girdi üzerinde bir **Aşama-1 ön adımı** olarak, yerel Aşama 1
zincirinden ve Graft/fTetWild katmanlarından önce çalışır; böylece bu katmanlar
Flap ile kapatılmış yüzeyi görür. Her halka en-küçük-alanlı bir dinamik
programla üçgenlenir, çevre kenar uzunluğuna göre inceltilir ve çemberin
konumunu ve eğimini yamaya taşıyan ayrık biharmonik (ince-plaka) bir çözümle
yumuşatılır. Özgün üçgenler aynen korunur -- yalnızca yeni yama eklenir -- ve
Flap'ın atladığı ya da reddettiği her halka olağan Aşama-1 düz delik doldurmaya
bırakılır, böylece her delik yine bir kapak alır.

**Varsayılan olarak açıktır.** `--no-flap` (veya `SUTURA_FLAP=0`) kapatır,
`--flap` (veya `SUTURA_FLAP=1`) açıkça zorlar; GUI aynı anahtarı Seçenekler →
Onarım sekmesindeki **Flap yüzey delik doldurma** onay kutusuyla sunar. Bir
aday yalnızca yeniden-yüklemede dürüst `delik + non-manifold` sayısı
kötüleşmediğinde benimsenir; ardından düzlük-tabanlı bir temizlik sıfır
kalınlıklı kalıntı tabakalarını atar, gerçek katı bir parçayı asla atmaz. İnce
plaka yaması reddedildiğinde (eş düzlemli katlanma) halka sırasıyla zar,
referans-yumuşatma ve yumuşatılmamış inceltilmiş adayları dener ve aynı
koruyucuları Flap zaman bütçesi içinde geçen ilkini uygular; bu halka-başına
seçimi kapatıp yalnızca ince-plaka davranışını korumak için
`SUTURA_FLAP_SELECT=0` ayarlayın.

Doldurma çekirdeği Rust uygulamasıdır (`sutura_geom.flap_fill`, `sutura_geom`
eklentisinin parçası) ve çalışma zamanının zorunlu bir gereğidir. Daha önce
yedek olarak kullanılan saf numpy oracle'ı kaldırılmıştır; Flap her zaman Rust
çekirdeğini kullanır. Topoloji ve rapor alanları değişmemiştir (yumuşatılmış
köşeler eski oracle ile yaklaşık 1e-8'e kadar örtüşür).

Rapor, `flap` anahtarını yalnızca anahtar açıkken taşır: `ran`, `adopted`,
`reason`, `baseline_holes`, `baseline_non_manifold`, `candidate_holes`,
`candidate_non_manifold`, `loops_found`, `loops_filled`, `loops_skipped`,
`loops_skipped_by_reason`, `patch_faces`, `new_vertices`, `time_s`,
`max_orig_vertex_move_mm` ve temizlik bir şey attığında `debris_cleanup`.

41 mesh'lik regresyonda ölçüm: Flap 13 mesh'te benimsendi ve koşu 41 mesh'in
tamamında yeniden-yüklemede su geçirmez sonuçlandı. Korpusta toplam Aşama-1
kendisiyle-kesişim sayısını 10.051'den 8.880'e düşürdü; ornate-frame'da
Aşama-1 kendisiyle-kesişimlerini 4.016'dan 3.005'e düşürdü; thingi10k_1038441'de en büyük yüzey sapması 4,11 mm'den
0,60 mm'ye indi. Rust motoru 41 mesh'lik korpusun tamamında Flap adımını
7,2 sn'de tutar.

### Tamponad (iç delik tıkacı, #17)

#17 Tamponad yöntemi (`sutura_engine.tamponade`, Rust `sutura_geom.tamponade`)
açık sınırları içeriden kapatır. Açıklığın üzerine yama oturtmak yerine her
sınır halkasını dolaşıp kabuğa en dar noktada kavuşan bir tıkaç kurar ve
çevredeki sağlıklı üçgenlere dokunmaz; girdi yüzleri birebir korunur
(`healthy_faces_kept`). Kalan kendini kesmeler daha sonra en derin noktadan
dışa doğru ince katmanlar hâlinde eritilir ve kesişme sayısını azaltmayan bir
katman geri alınır, böylece katman hiçbir zaman daha kötü bitmez. Flap ve
Graft'ın içe dönük karşılığıdır. Sınır halkası yürüyüşü ve referans yüzeyi
halkası kanonik bir köşe sırası kullanır; bu yüzden halka ayrıştırması ve
seçilen tıkaç, girdi üçgenlerinin saklanma sırasına bağlı değildir.
**Temizlenmiş girdi üzerinde, Flap ve Aşama 1'den önce bir ön-geçiş olarak**
çalışır ve **Aşama 1/Flap kapatma merdiveninden sonra, Graft/fTetWild
katmanlarından önce ikinci bir şans** alır.

**Varsayılan olarak açıktır**: kapatmak için `--no-tamponade` (veya
`SUTURA_TAMPONADE=0`) ya da Seçenekler → Onarım sekmesindeki **Tamponad
(delikleri içeriden doldur)** onay kutusu. Bir tıkaç yalnızca şu durumlarda
reddedilir, böylece katman yalnızca sonucu kesin olarak iyileştirdiğinde çıktıyı
değiştirir:

- **katı cismin dışına sızar** — tıkacın bir örnek noktası orijinal yüzeyden
  `clamp(köşegen/10000, 0,0125, 0,05)` mm'den daha uzakta boş uzayda yer alır ve
  delik ağzının üzerinde değildir;
- yeniden-yüklemede dürüst (kaynaklı) mesh'te **delik veya non-manifold kenar
  oluşturur**;
- mevcut bir yüzeyle **ters yönde çakışır** (çift duvar) ve bu çakışma kendi
  alanının %20'sinden fazladır — ters yönlü çakışan yüzler tek-çift dilimleyicide
  malzemeyi iptal edebilir; aynı yönlü çakışma ise pozitif dolgulu dilimleyicide
  birleştirilir ve olduğu gibi bırakılır;
- **ayrı bir parça veya kendi başına kapalı bir kabuk ekler** (tıkaç, sınır
  halkası üzerinden ana kabuğa kenar-bağlantılı olmalıdır);
- ya da katman öncesi yüzlerin %99'undan azı korunur.

Katman içinde, referans tıkacı reddedilen bir halka içten şişirilen bir aday
kazanır, **Stent**: tıkaç içe doğru büyütülür ve kenarın üzerine asla
yükselmeyecek şekilde sınırlanır (düz bir geçiş deliği kubbeye dönüşemez ve bir
Stent tıkacı her zaman orijinal kenar üzerinde kurulur, asla bir referans taşma
düzeltmesi üzerinde değil). Tek bir Stent'in kapatamadığı bir halka, sıkışma
noktalarında ve en kısa geçerli iç kiriş boyunca basit alt halkalara bölünür ve
her alt halka için bir Stent şişirilir; komşu alt tıkaçlar kiriş kenarını birebir
paylaşır, böylece dikiş manifold kalır ve grup ya-hep-ya-hiç olarak değerlendirilir.
Ad, #8 Balon (native örtük yeniden inşa) yöntemiyle karışmaması için
bilinçli olarak *balon* değildir. Stent yalnızca Tamponad çalışırken çalışır;
tek anahtarı `SUTURA_TAMP_STENT=0` ortam vazgeçmesidir (varsayılan açık, geometri
açısından no-op).

Benimseme kapısı topolojik olduğundan, temiz ama biçimi değiştiren ya da parça sayısını düşüren bir tıkaç yine de geçebilir; bu yüzden son bir güvenlik ağı, katman sonrası sonucu katman öncesi sonuçla yeniden-yüklemede dürüst mesh üzerinde karşılaştırır ve katman kesin olarak iyileştirmediyse Tamponad kapalı olarak yeniden çalıştırır; böylece katman nihai modeli `--no-tamponade` durumundan asla daha kötü yapamaz.

Kendini kesmeler **yalnızca raporlanır** (dilimleyici çakışan hacimleri
birleştirir), bu yüzden benimsenmeyi hiçbir zaman engellemez. Rust eklentisinin
`sutura_geom.tamponade` sunması gerekir (numpy için yedek yol yoktur); aksi halde
katman kendini kullanılamaz olarak bildirir ve atlanır. Süre bütçesi yoğunluk ön
ayarını izler (`quick` bu katmanı kapatır). Rapor `tamponade` anahtarını taşır:
`ran`, `adopted`, `reason`, `plugs`, `plug_faces`,
`melt_layers_tried`/`_accepted`/`_rolled_back`, `micro_welds_ok`/`_failed`,
`healthy_faces_kept`, `input_faces`, `output_faces` ve `seconds`.

### Splint (kesin katı yeniden inşa, #18)

#18 Splint yöntemi (`sutura_engine.pipeline.tiers.splint`, Rust çekirdeği
`sutura_geom.splint_cut`) derin onarım merdiveninin **son çare** katmanıdır:
girdi yüzeyinin işaretsiz-mesafe proxy'sini kurar, Delaunay ile dörtyüzlülere
ayırır ve iç kısmı kesip atarak yapısı gereği su geçirmez bir katı sınır üretir.
Deterministiktir ve eklenti gerektirmez (fTetWild eki gerekmez); **Pansuman'dan
sonra ve fTetWild'dan önce** denenir.

Splint **isteğe bağlıdır ve varsayılan olarak kapalıdır** (`--experimental-splint`
/ `SUTURA_SPLINT`; kapatmak için `--no-splint` / `SUTURA_SPLINT=0`; kütüphane
varsayılanı `deep_repair=None` durumunu bayt düzeyinde korur). Bir aday yalnızca
yeniden yüklendiğinde kesin su geçirmez olduğunda benimsenir; ham kesim kapalı
ama iki-manifold değilse önce Stage-2 Manifold köprüsüyle yeniden inşa edilir ve
bu yeniden inşa yalnızca delik ve non-manifold kenar sayısında daha kötü
olmadığında tutulur. Maliyeti bir çözünürlük üst sınırıyla (`SPLINT_RES=128`) ve
bir girdi-yüz tavanıyla sınırlıdır; duvar-saati bütçesi dışsal bir tümünü-ya-da-
hiçbirini iptalidir (Rust kesimi tek, kesilemeyen bir çağrıdır). Rapor `splint`
anahtarını taşır: `ran`, `adopted`, `reject_reason`, `input_faces`,
`output_faces`, `output_parts`, `out_manifold`, `cut_shells`,
`construct_status`, `manifold_postprocessed`, `res`, `adaptive*` ve `seconds`.

### Koaptasyon (dikiş kaynağı birleştirme, #19)

#19 Koaptasyon yöntemi (`sutura_engine.pipeline.tiers.coaptation`, Rust
çekirdeği `sutura_geom.close_holes` + `union_pair`), katlanmış/üst üste binen
çok-gövdeli bir çorbayı hedefler — örneğin aşama-1 zincirinin üst üste binen
iki yarım olarak kapattığı ince bir tabaka. Böyle bir sonuç, dikişleri yaklaşık
bir float32 ULP ile ayrılmış birkaç kenar-bağlantılı gövde olarak yeniden
yüklenir ve gövdelerin kesiştiği yerde kendini kesmelerle dolu, kapalı ama
bozuk kalır; bu yüzden Graft / Pansuman / Splint / fTetWild hiç çalışmaz.
Koaptasyon **orijinal girdi dizileri** üzerinde çalışır (Graft/fTetWild/Splint
gibi), dikişi kaynatır (yalnızca `COAPTATION_SEAM_ULP_K=1` float32 ULP
içindeki sınır-kenarı köşeleri birleştirilir — iç köşeler asla birleştirilmez,
böylece gerçek bir ince duvar çökemez), her gövdeyi kapatır ve bunları kesin
birleştirme ile katlar.

**Varsayılan olarak açıktır** (zorlamak için `--coaptation`, kapatmak için
`--no-coaptation` / `SUTURA_COAPTATION=0`; açık anahtarı
`COAPTATION_DEFAULT_ENABLED`/`SUTURA_COAPTATION_DEFAULT`) ve **Splint'ten
sonra, fTetWild'den önce** yer alır. Bir aday yalnızca merdivenin şu an
tuttuğu sonuçtan **kesinlikle daha iyi** olduğunda benimsenir: yeniden-yükleme
su geçirmez, kesin kendini kesme 0 ve temel değerin ölçülebilir sayısının
altında, ve delik/non-manifold kenar/bileşen sayısında daha kötü değil. Aksi
halde merdiven sonucu bayt düzeyinde aynı döndürülür, böylece fayda
sağlayamayan bir mesh değişmez. Maliyeti bir girdi-yüz tavanıyla
(`COAPTATION_MAX_FACES`, `skipped_budget`/`reject_reason='too_large'`) ve
dışsal, ya-hep-ya-hiç duvar-saati iptaliyle (`COAPTATION_TIMEOUT`)
sınırlandırılır. Rapor `coaptation` anahtarını `ran`, `adopted`, `gate`,
`ulp_k`, `tol`, `bodies_in`, `bodies_after_weld`, `merged`, `baseline`,
`candidate`, `input_faces`, `output_faces`, `skipped_budget`, `aborted`,
`reject_reason`, `reason`, `warnings` ve `seconds` ile taşır.

### Graft (morfoloji kabuk sarma, #13)

#13 Graft yöntemi (`sutura_engine.graft`, eski adıyla `sutura/shell_wrap.py`),
ağır hasarlı modellerde fTetWild öncesinde otomatik olarak devreye giren yüksek
sadakatli bir morfoloji kabuk sarma katmanıdır.

> **Not:** Graft (#13), `sutura_geom` Rust eklentisini gerektirir (`install.sh`,
> `install-macos.sh` tarafından otomatik kurulur ve AppImage/.dmg paketlerine
> dahildir). Eklenti zorunludur: bulunmadığında kurulum başarısız olur ve CLI
> adlandırılmış bir hatayla başlamayı reddeder; Graft/fTetWild'a geri dönüş yolu yoktur.

1. **Genelleştirilmiş Sarımlı İşaretli Mesafe**: Rust `sutura_geom` çekirdeğiyle,
   sağlıklı üçgenlerin medyan kenar uzunluğuna uyarlanmış bir voksel ızgarası
   üzerinde işaretli mesafe alanı (SDF) oluşturulur.
2. **Morfolojik Kapatma**: Yüzeye önce morfolojik genişletme (dilation), ardından
   gerçek Öklid Mesafe Dönüşümü (EDT) ve erozyon uygulanır. Kapatma yarıçapı
   ~1.5 vokselden başlar ve yalnızca dual-contouring çıktısı yeniden yüklemede
   su geçirmez olana kadar dinamik olarak genişletilir.
3. **Birebir Hibritleştirme**: Çıkarılan kabuk zarfı rafine edilip özgün yüzeye
   izdüşürülür. Sağlıklı bölgelerdeki üçgenler girdi dosyasındakiyle bayt-birebir
   korunurken, yalnızca hasarlı açıklıklar zarf ile kapatılır.
4. **Sadakat Koruması ve Geri Dönüş**: Sonuç yalnızca yeniden yüklemede su geçirmezse
   kabul edilir. Sağlıklı yüzeydeki sapma eşiği aşarsa (`fidelity_ok == False`),
   otomatik triyaj fTetWild'ı da dener ve sağlıklı yüzey sapması daha düşük olanı
   benimser. Ayrıntı kaybı raporda (`graft.warnings`) gösterilir.
5. **Belirleyici çözünürlük**: Izgara bütçesi yalnızca açık yoğunluk ön ayarının
   (Quick/Balanced 1,5M hücre, Thorough 10M, Extreme 25M) saf bir fonksiyonudur; aynı
   girdi ve seçenekler her makinede aynı zarfı üretir. Boş RAM hiçbir zaman keyfî bir
   çözünürlük seçmez: ön ayar bütçesi sığmazsa Graft bir kez Balanced ızgarasına geri
   döner (çıktıda bildirilir) ve yalnızca bunun da sığmadığı durumda açık bir
   `low_memory` mesajıyla atlanır.
6. **Belirleyici kapama merdiveni**: Kapatma yarıçapı merdiveni, duvar saatine göre değil
   girdi ızgarasından ve ön ayardan türeyen belirleyici bir iş bütçesine göre durur; aynı
   girdi ve ön ayar hızlı ya da yüklü makinede aynı ağı verir. Duvar-saati bütçesi yalnızca
   dış bir güvenlik kesintisidir: tetiklenirse Graft tümüyle bırakılır, Aşama 1 sonucu
   korunur ve hem rapor hem GUI bunu açıkça söyler.
7. **Girdi kapsama koruması**: bir zarf, yalnızca girdinin gerçek boyutunun en az yarısını kapsıyorsa kabul edilir (`graft.GRAFT_MIN_INPUT_COVERAGE = 0.5`; rapor `input_coverage` ve `coverage_ok` taşır); böylece kalan yeri doğru olsa da çökmüş bir sonuç reddedilir ve Aşama 1 sonucu korunur.

### Pansuman (değişken viskoziteli hacimsel kaplama, #16)

#16 Pansuman (`sutura_engine.dressing`), Graft'ın cerrahi kardeşidir: hasarlı
bölgeyi köprülemek yerine parçanın tamamını uzamsal olarak değişen bir
"sıvıya" batırır ve `F(x) = s(x) - r(x)` seviye kümesini çıkarır; burada `s`
genelleştirilmiş sarımlı işaretli mesafe, `r` ise ayrıntılı sağlıklı yüzeyde
ince kalan ve hasar bölgesinde kalınlaşan viskozite yarıçapıdır. Bir skaler
alanın izo-yüzeyi yapısı gereği 2-manifold ve kendisiyle-kesişimsizdir; bu,
Pansuman'ı yerel yama eksizyonunun ve hibrit eklemenin başarısız olduğu
tüm-kabuk topolojik katlanmaları için doğru araç yapar.

1. **Özellik-çıpalı çözünürlük**: voksel boyutu mesh'in karakteristik özellik
   boyutunu (medyan kenar uzunluğu) takip eder ve yoğunluk ön ayarının çapraz
   bandına kırpılır; böylece kaba bir açık parça aşırı örneklenmez (önceki
   `diag/200` kuralı 71 yüzlü bir mesh'te 1 M yüzlü kaplama ve %7 sapma
   üretiyordu).
2. **Değişken viskozite**: temel yarıçap incedir (sadakat yüzeye yapıştırmadan
   değil, ince kaplamadan gelir — kaplamayı girdi üzerine geri izdüşürmek
   binlerce kendisiyle-kesişim üretip kalıcı olarak reddedilmiştir), köprüleme
   yarıçapı ön ayarla ve en büyük boşluğun %45'iyle sınırlıdır.
3. **Numpy prototip arka ucu, Rust'a hazır**: alan kurulumu ve yürüyen
   tetrahedra bugün numpy'de çalışır; Rust `sutura_geom.dressing_coat` dar-bant
   çekirdeği özellik-algılamayla bulunur ve mevcut olduğunda otomatik devralır.
   Sadeleştirme, bir delik açacak, non-manifold kenar oluşturacak ya da tam
   kendisiyle-kesişim ekleyecek olursa atomik olarak geri alınan yerel
   (meshoptimizer) bir quadric kenar-çökeltmesidir.
4. **Boşaltma (sağlıklı bölge geri aşındırma)**: kaplama özgün yüzeyin ~`r_base`
   dışında oturur; bu yüzden seviye kümesinin **içine** bir boşaltma terimi
   eklenir (`F = s - r + delta_r`) — 7.850 kendisiyle-kesişim üreten tepe
   izdüşümüyle asla. Tam boşaltma sağlıklı büyümeyi ~%40 azaltır (medyan
   +0,33 → +0,20 mm) ve zararsız ~4 µm medyan içe çökme bırakırken kusurlar
   kaplı kalır. `--dressing-drain MODE|mm` / `SUTURA_DRESSING_DRAIN` / GUI
   *Boşaltma* açılır listesi ön ayar varsayılanını geçersiz kılar (Hızlı
   kapalı, Dengeli/Titiz tam, Aşırı derin).
5. **Tek bir varsayılan anahtarın arkasında Auto-merdiven yedeği, KAPALI
   gönderilir.** #13 Graft (kelimesi kelimesine hibrit) yöntemi, sağlıklı orijinal
   üçgenleri mümkün olduğunca aynen koruyarak Aşama 1'den sonraki varsayılan otomatik
   onarım adımı olarak kalır. Anahtar açıkken (`DRESSING_DEFAULT_ENABLED` /
   `SUTURA_DRESSING_DEFAULT=1`) Auto merdiveni Dressing'i Graft'tan sonra ve
   fTetWild'dan önce isteğe bağlı bir yedek olarak dener; `--si-mode repair` altında
   pozitif bir kesin self-intersection sayısı da katmanı tetikler (varsayılan
   `--si-mode report` altında yalnızca kalan delikler ve non-manifold kenarlar
   yedekleri tetikler). `--experimental-dressing` / `SUTURA_DRESSING=1` / GUI onay
   kutusu *Pansuman (viskozite kaplaması)* çalışmaya **zorlar** (uyarı:
   *watertight, but the shape may deform / fine detail may be lost*);
   `--no-dressing` / `SUTURA_DRESSING=0` kapatır. Benimseme katı-su geçirmez,
   tam-SI'sız, ön ayarın kaplama→girdi sadakat kapısı ve aşağıdaki şekil-koruma
   kapılarını gerektirir. **Anahtar varsayılan olarak KAPALIDIR**: ince-detaylı bir kabartmada (ornate-frame) voxel merdiveni,
   ortalama sapma ~0,08 mm kalmasına rağmen olukları görünür biçimde düzleştirir,
   bu yüzden bu kalite sorunu düzeltilene kadar Dressing böyle parçalarda
   kelimesi kelimesine Graft/fTetWild sonucunun yerini almamalıdır.
6. **Şekil-koruma kapıları.** Sadakatin yanı sıra bir aday, yalnızca Dressing'den
   önce merdivenin tuttuğu sonuca göre: işaretli hacim en fazla
   `DRESSING_MAX_VOLUME_DELTA` (%10) değişiyorsa, bağlı-bileşen sayısı en fazla
   `DRESSING_MAX_PARTS_SLACK` (1) artıyorsa ve sağlıklı yüzey üzerinde P95
   kaplama-vs-girdi **tepe-normal** açısı `DRESSING_MAX_NORMAL_ANGLE_P95` (30°,
   bir yer tutucu — ölçülen kaplama P95'i kabul edilen mesh'lerde 12–36°, detay
   kayıplı karelerde 82–88°) altında kalıyorsa benimsenir. Normal metriği (KD-tree
   ile en yakın girdi tepe noktası, sağlıklı = kusur tepe noktası değil) yalnızca
   konuma bakan Hausdorff kapısının yakalayamadığı merdiveni yakalar; hacim/parça
   kapıları bir oyuğu yutan veya artıklara dağılan kaplamayı yakalar. Geometrik
   CAD/işlenmiş-vs-organik puanı (`cad_likeness`) her Auto çalışmasında kaydedilir
   (gelecekteki kalibre CAD koruması için); merdiven zaten tüm girdilerde önce
   Graft'ı çalıştırır, böylece CAD benzeri bir parça Dressing'den önce kelimesi
   kelimesine Graft hibritini alır ve Dressing yalnızca hasar kaldığında çalışır.
7. **Kusur kümesi ve viskozite geçersiz kılmaları**:
   `--dressing-defects {all,holes_nm}` (`SUTURA_DRESSING_DEFECTS` / GUI *Kusurlar*
   açılır listesi) viskozite maskesini süren girdi kusurlarını seçer — `all`
   (varsayılan) delikler, non-manifold kenarlar ve kendisiyle-kesişimler;
   `holes_nm` kendisiyle-kesişim terimini düşürür, böylece tek hasarı delik /
   non-manifold kenar olan bir tarama sözde SI ile kalınlaştırılmaz.
   `--dressing-rmax-scale F` / `--dressing-sigma-scale F`
   (`SUTURA_DRESSING_RMAX_SCALE` / `SUTURA_DRESSING_SIGMA_SCALE` / GUI *r_max* ve
   *sigma* açılır listeleri) çözülen `r_max` / `sigma` değerlerini çarpar; varsayılan
   (ayarsız = 1,0) bayt düzeyinde aynıdır ve sıfır/negatif ya da ayrıştırılamayan
   değerler yok sayılır.
8. **Bileşen temizliği**: küçültmeden önce çıkarılan üçgen çorbası köşe bağlantılı
   bileşenlere ayrılır ve çıkarım artıkları — yüz sayısı, voxel hacmi ve göreli
   hacim tabanlarının altındaki alt-voxel kabuklar — atılır. Ayakta kalan bir ters
   kabuk yalnızca korunan bir madde kabuğunun içinde yuvalanmışsa *ve* girdinin
   genelleştirilmiş sarmalım sayısı merkezinde ~0 ise (yani girdi orada gerçekten
   boşsa, gerçek bir oyuk) tutulur. 13 mesh'lik `dressing_tuned` setinde bu,
   `thingi10k_145065`'i 669 bileşenden 1'e ve `thingi10k_63785`'i 25'ten 1'e
   indirdi (sonrasında OrcaSlicer'da her ikisi de `number_of_parts = 1`) ve
   `thingi10k_1038439`'un üç gerçek madde yumrusunu korudu; atılan sayılar ve hacim
   `report['dressing']['cleanup']` içinde raporlanır.
9. **İsteğe bağlı öneri ve `--dressing-force-adopt`.** Düz bir Auto onarımı
   geometriyi hâlâ açık veya non-manifold bıraktığında (varsayılan `--si-mode report`
   altında), rapor bir `suggestions` listesi taşır (`method = dressing`; kusur
   `issues`'ından ayrıdır, böylece kategori değişmez). `--human` raporu işaretli bir
   ipucu basar (`⚠ Not watertight…`): Dressing hiç çalışmadıysa
   `--experimental-dressing`'e:
   `  ⚠ Not watertight. You can try Dressing: --experimental-dressing (watertight, but the shape may deform / fine detail may be lost).`
   Dressing çalışıp kaplaması şekil kapıları tarafından reddedildiyse `--dressing-force-adopt`'a:
   `  ⚠ Not watertight. Dressing ran but its result was rejected by the quality gate (...).`
   `    Force it with --dressing-force-adopt if you accept the deformation (the shape may deform).`
   işaret eder (parantez içinde başarısız hacim / normal sayılarıyla).
   `--dressing-force-adopt` `--experimental-dressing`'i gerektirir ve hacim /
   bileşen / normal / sadakat kapılarını atlar; yine de kesin su geçirmez ve
   kendisiyle-kesişimsiz bir aday şarttır; kaydın `forced` alanı kapıların
   atlandığını işaretler. GUI aynı bayrağı toplu bir Options → Deneysel onay
   kutusu *Pansuman: zorla benimse (deforme olabilir)* olarak sunar ve herhangi bir
   sonucun ipucu verdiği bir toplu işten sonra, tam o dosyaları bayrakla yeniden
   çalıştıran (ve onay kutusunu işaretleyen) engellemeyen bir **⚠ Dressing ile dene
   (su geçirmez yapar, ama parça deforme olabilir)** (veya *⚠ %d dosyada Dressing ile dene
   (su geçirmez, ama parça deforme olabilir)*) düğmesi gösterir.

### Dürüst su geçirmezlik kararı (kaydet/yeniden yükle sonrası doğrulanır)

STL hiçbir tepe paylaşımı saklamaz; bu yüzden bellekteki indeks topolojisinde su
geçirmez olan bir mesh, yükleyici tam-çakışık `float32` konumları yeniden
kaynakladığında manifold olmayan kenarlarla yeniden yüklenebilir — 40 gerçek
dünya örneğinin 7'sinde ortaya çıkan bir yanlış "su geçirmez" iddiası sınıfı
(örn. `thingi10k_1038439`). Sutura artık bellekteki dizileri değil, **kaydedilen**
mesh'i yargılar:

- **P-HONEST** (`repair.enforce_reload_verdict`) son mesh'i kaydet/yeniden
  yükle eşdeğer biçimde yeniden kaynaklar (konumlar `float32`'ye dökülür, tam
  kopyalar birleştirilir) ve bir raporun su geçirmez demesine yalnızca bu biçim
  katı-su geçirmez olduğunda izin verir. Yanlış bir iddia düşürülür
  (`reload_watertight = false`, `stage1.reload_holes` / `reload_non_manifold`,
  `stage2.watertight_after_reload = false`) ve yöntem kayıt defteri aynı kuralı
  uygular; böylece böyle bir temel başarısız sayılır ve otomatik yol ek denemeye
  geçer. Gerçekten su geçirmez bir mesh raporunu ve çıktısını bayt-birebir korur.
- **P-WELD** (`repair.p_weld_final`), dikişi yalnızca bildirmek yerine düzelten
  yeniden-yükleme güvenli son geçiştir. Bellekteki topoloji zaten temiz bir
  2-manifold olduğunda, `float32`-çakışma tepe gruplarını bir-ULP-altı kaydırmayla
  ayırır (yüz sayısı korunur, geometri milyonda birden az oynar); kaynaklanmış
  mesh'in kendisi onarılabilirse topoloji onarımı artı küçük-delik kapatmaya geri
  düşer. Bir aday yalnızca kaydedilen mesh katı-su geçirmez yeniden yüklendiğinde
  benimsenir, böylece geçiş çıktıyı asla kötüleştiremez. `report['p_weld']`
  öncesi/sonrası delik ve manifold-olmayan sayılarını, yöntemi ve kaydırılan tepe
  sayısını kaydeder; `--human` bir *Reload-safe final pass* satırı yazdırır.

P-WELD ile daha önce `warning` olan yedi örnek artık değişmemiş yüz sayılarıyla
katı-su geçirmez yeniden yüklenir — `thingi10k_100281`, `1038439`, `1038441`,
`145065`, `224108`, `248395`, `71691` — zaten su geçirmez olan mesh'ler ise
dokunulmadan bırakılır (`p_weld` yok).

## Harici onarım motorları ve fTetWild yöneticisi

Sutura, kullanıcının kurduğu üçüncü taraf onarım **motorlarını**
çağırabilir ve isteğe bağlı fTetWild yedek ekini aynı arayüzden (CLI ve GUI)
yönetebilir. Bu Triage Engine katmanının iki parçası da isteğe bağlıdır ve
yapılandırılana kadar etkisizdir.

**Harici motorlar.** Bir motor,
`${XDG_CONFIG_HOME:-~/.config}/sutura/engines/*.toml` altındaki küçük bir TOML
dosyasıdır; bir çalıştırılabilir dosya ve argümanlarını (`{input}`,
`{output}` ve isteğe bağlı `{tmpdir}` yer tutucuları) tanımlar. Motorlar
zincire bir `placement` ile yerleşir — `before_stage1`, `after_stage1`,
`after_stage2`, `replace_ftetwild` veya `final_fallback` — ya da yerleşik
aşamaları (`stage1`, `stage2`, `deep_repair`, `ftetwild`) ve motor adlarını
sıralayan bir `chain.toml` ile açıkça sıralanır; bilinmeyen bir ad tüm
zinciri reddeder ve varsayılana döner. Komutlar kabuk olmadan, kendi süreç
grubunda çalışır ve zaman aşımında öldürülür. **Her motor sonucu fTetWild ile
aynı korumadan geçer** — yalnızca delikleri ve non-manifold kenarları mevcut
sonuçtan daha kötü değilse uygulanır ve girdiden çok uzaktaki bir sonuç
reddedilmek yerine `shape_changed` olarak işaretlenir (Hausdorff >
`FTETWILD_MAX_HAUSDORFF_REL`); bir motor korumayı asla atlayamaz veya devre
dışı bırakamaz. Her koşu motor başına bir rapor girdisi ekler (`name`,
`placement`, `rc`, `time`, `adopted`, `reject_reason`, stdout/stderr
kuyrukları) ve bozuk yapılandırmalar yalnızca `engine_warnings` içinde uyarır,
onarımı asla başarısız kılmaz. Motor tanımlı değilse onarım motorlar öncesi
davranışla byte-byte aynıdır. `sutura engines list` çözülen zinciri ve her
motorun yerleşimini, etkinlik durumunu ve ikilisinin bulunup bulunmadığını
gösterir; `sutura engines check` ayrıca yapılandırılmış bir ikili eksikse
sıfırdan farklı çıkış verir (motoru asla çalıştırmaz).

**fTetWild yöneticisi.** `sutura ftetwild status|install|uninstall`, fTetWild
ekini (`pytetwild` + `pyvista` + `vtk`) yalnızca Sutura ortamında (Linux
`venv311` veya macOS conda `sutura-env`) inceler, kurar veya temiz biçimde
kaldırır; önce indirme ve disk boyutlarını gösterir ve kurulum/kaldırma için
`--yes` (veya etkileşimli onay) gerektirir; `--dry-run` planı yazdırır ve
ortama asla dokunmaz. Desteklenmeyen düzenleri (AppImage, donmuş `.app`/`.dmg`,
sistem Python'u, salt-okunur ortamlar) açık bir gerekçeyle reddeder ve
kaldırma yalnızca ekin eklediği paketleri budayıp temel ortamı korur. GUI'de
bu, Seçenekler penceresinin **Motorlar** sekmesidir: durum satırı
(kurulu/sürüm/boyut veya desteklenmeyen gerekçesi), onayda boyutu gösteren
Kur/Kaldır ve akışlı, iptal edilebilir bir ilerleme penceresi, ayrıca
yapılandırılmış motor listesi ile Yenile, *Motor klasörünü aç* ve
`docs/external-engines.md` bağlantısı. İş parçacıkları kesilebilir ve pencere
kapanırken beklenir.
