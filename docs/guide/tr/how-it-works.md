# Sutura nasıl çalışır

[← README'ye dön](../../../README.tr.md) · [English](../how-it-works.md)

## Sutura bir modeli nasıl onarır, sade anlatım

Bozuk bir 3B model hasarlı bir deri gibidir. Tarayıcı göremediği yerlerde
**delik** bırakır, üçgenler **yanlış yerlerden birbirine yapışır** (manifold
olmayan kenarlar) ve yüzeyin bazı kısımları **birbirinin içinden geçer**
(kendisiyle kesişme). Dilimleyiciler böyle bir dosyayı ya reddeder ya da yanlış
basar.

Sutura dikkatli bir cerrah gibi çalışır: hasarı tedavi eder, sağlam deriye
dokunmaz. Onarım kısa bir adım zinciridir ve her adım yalnızca önceki adımlar iş
bıraktıysa devreye girer:

```
dosyan ─► Tamponad ─► Flap ─► Aşama 1 temizlik ─► (hâlâ açık mı?) Tamponad ─► (hâlâ bozuk mu?) Graft ─► Pansuman/Splint (isteğe bağlı) ─► Koaptasyon (açık) ─► fTetWild (kuruluysa) ─► Aşama 2 katı ─► onarılmış kopya
          delikleri   delikleri    kenarları düzeltir,            delikleri                    yalnızca hasarlı    tek parça, kapalı,
          içeriden    yüzeyi       kalan delikleri                içeriden                     bölgeleri yeniden   basılabilir katı
          tıkar       sürdürmerek  kapatır                        tıkar                        kurar
                      kapatır
```

1. **Tamponad: delikleri içeriden, lav gibi tıkar** (varsayılan açık).
   İlk o çalışır: temizlenmiş girdi üzerinde Flap ve Aşama 1'den önce bir
   ön-geçiş olarak, böylece gerçek bir krater düz bir kapak yerine hemen
   kabuğun en dar noktasında tıkanır. Parçayı içeriden doldurur: parçanın
   içinin lavla dolduğunu düşün. Kabuk sağlamsa lav oraya yalnızca dokunup
   durur, yani orijinal yüzey değişmez. Bir delikte lav dışarı kaçacaktır; bu
   yüzden hemen kabuğun en dar yerinde tutulup sızdırmaz hale getirilir ve
   donan lav tıpayı oluşturur. Parçanın içinde kalan kıvrımlar en derin
   noktadan dışa doğru, her seferinde bir katman eritilir; bir katman hatalı
   bir kenar (iki yüzün yanlış biçimde paylaştığı, yani manifold olmayan bir
   kenar) oluşturacaksa o son katman geri alınır ve onun yerine çok küçük bir
   nokta kaynakla kapatılır. Flap ve Aşama 1'in bıraktığı delikler için ikinci
   bir şans daha alır ve yalnızca sonuç ölçülebilir biçimde daha iyiyse
   tutulur. Adı cerrahiden gelir: tamponad, kanamayı ağzında durdurmak için bir
   boşluğu içeriden doldurur. Varsayılan olarak açıktır; kapatmak için
   `--no-tamponade` ya da GUI Seçenekler
   kutusu. Tamponad içinde **Stent**, bir döngü için referans tıkaç reddedildiğinde
   kullanılan, içeriden şişerek kapanan bir tıkaçtır; kabuğa dayanınca durur ve
   katının dışına asla taşmaz. Yöntem #8 Balloon (native örtük yüzey yeniden
   oluşturma) ile ilgisi yoktur.
2. **Flap: delikleri yüzeyin gittiği yöne doğru doldurur** (0.8.0'da yeni,
   varsayılan açık). Klasik delik doldurma deliğe düz bir kapak koyar. Flap ise
   çevredeki yüzeyi deliğin içine doğru devam ettirir; bir boşluğun üstüne sıvı
   astar ya da epoksi sürmek gibi: yama, kenardan komşu yüzeyle aynı eğimle
   çıkar, böylece kavisli bir yüzeydeki delik çukur ya da düz bir leke değil,
   kavisli bir yama alır. Orijinal üçgenlerin yerinden oynamaz; yalnızca yama
   yenidir. Flap bir deliği güvenle dolduramıyorsa onu bir sonraki adıma bırakır,
   yani bir dosya Flap ile asla Flap'siz halinden kötü olmaz. Kapatmak için
   `--no-flap` ya da *Flap yüzey delik doldurma* kutusu.
3. **Aşama 1: temizlik (yerel).** Tekrarlanan ve bozuk üçgenleri siler,
   hatalı kenarları çözer, tüm yüzleri aynı yöne çevirir ve hâlâ açık kalan
   delikleri kapatır.
4. **Graft: yalnızca hasar ciddiyse.** Model hâlâ açık ya da karışıksa Graft,
   hasarlı bölgelerin üstüne yeni ve kapalı bir kabuk kurar ve bunu sağlam
   orijinal yüzeye deri nakli gibi diker. Sağlam bölgeler orijinal üçgenlerini
   korur; böylece oyma ya da süsleme gibi ince ayrıntılar kaybolmaz. Çok büyük
   taramalar için boyut ve süre sınırı vardır, takılıp kalmaz.
5. **Aşama 2: tek bir katı yap (Manifold).** Sonucu geçerli, kapalı bir katı
   olarak yeniden kurar; Bambu Studio'nun kullandığı kütüphanenin aynısı.
6. **İsteğe bağlı ekler, çoğunlukla varsayılan kapalı.** **Pansuman (Dressing)**
   tüm modeli her zaman kapalı çıkan sanal bir kaplamaya batırır, ama ince
   ayrıntıyı yumuşatabilir; bu yüzden Sutura onu yalnızca her şey başarısız
   olduğunda *önerir*, karar senindir; **Splint** isteğe bağlı tam katı yeniden
   inşadır. **fTetWild** çok bozuk dosyalar için son çare yeniden kurulumdur ve
   varsayılan olarak kuruludur; stage 1 hâlâ delik bıraktığında otomatik çalışır.
   Deneysel bir kesin yeniden inşa (CDT, `--solid-engine cdt`) onun yerini
   alabilir; CDT başarısız olur ya da zaman aşımına uğrarsa yine fTetWild
   kullanılır.

Sutura her adımdan sonra delikleri ve hatalı kenarları, dosya diskten yeniden
açılmış gibi tekrar sayar; işleri kötüleştiren adım çöpe atılır. Orijinal
dosyan asla değişmez: onarılmış bir kopya ve tam olarak hangi adımların
çalıştığını gösteren bir rapor alırsın. Dilimleyicilerin görmezden geldiği küçük
kendisiyle kesişmeler raporlanır ama artık hata sayılmaz (`--si-mode repair`
katı davranışı geri getirir).

### Adımların arkasındaki yardımcılar

Numaralı adımlar görünen onarımdır. Çevrelerinde ne yapılacağına karar veren ve
sonucu denetleyen birkaç yardımcı vardır:

- **Teşhis (Diagnosis)** onarımdan önce modele bakar ve parçanın türünü anlar —
  işlenmiş mekanik bir parça, pürüzsüz organik bir şekil, taranmış bir yüzey ya
  da bir kafes — böylece sonraki adımlar uygun ayarı seçebilir. İlk o çalışır.
- **Triyaj (Triage)** bu teşhisi ve hasarın türünü bir plana çevirir: hangi
  onarım yöntemleri hangi sırayla denenecek ve her birine ne kadar süre
  harcanacak. Adımların arasında çalışır ve çok uzun sürecek yöntemleri eler.
- **Chart** önceki işlerin önbelleğidir; girdinin şekline göre saklanır. Ağır
  adımlardan önce bakılır, böylece aynı geometriyi (ya da tek bir dosyanın
  içindeki nesneleri) yeniden onarmak hızlıdır.
- **X-Ray** dürüst bitiş denetimidir. Bellekte tutulan sayılara güvenmek yerine,
  az önce yazılan dosyayı yeniden açar ve delikleri ve hatalı kenarları bir
  dilimleyicinin yapacağı gibi sayar. Onarımın sonunda çalışır.
- **Stitch** bu denetimden hemen önce çalışır. Bir dosya bellekte kapalı görünüp
  de yazılırken koordinatları yuvarlandığı için küçük bir açıklıkla geri
  yüklenebilir; Stitch hiçbir yüzü oynatmadan o dikişleri iyileştirir.
- **Hull** dış yüzeyi çıkarır. Model birbirine değen ya da iç içe geçen
  parçalardan oluşuyorsa yalnızca dış yüzeyi bırakır, böylece iç yüzler hataya
  dönüşmez. Teşhis bir birleşim tanıdığında ön adım olarak çalışır.
- **Cast** Sutura'nın kendi hızlı geometri motoruna (küçük bir Rust yardımcısı)
  açılan köprüdür. Yalnızca kesin ölçüm gereken bir adım çalışırken yüklenir;
  örneğin kesin kendisiyle-kesişme bölme işlemi ya da Graft ve Dressing
  kabukları.

Onarım katmanlarının kendisi — Flap, Tamponad, Graft, Pansuman (Dressing),
Splint ve fTetWild — yukarıdaki adımlarda anlatılmıştır.

## Tekrarlanabilir sonuçlar

Çıktıyı değiştiren onarım kararları geçen süreyi değil sayılan işi kullanır.
Aynı dosya ve aynı onarım ayarları hızlı, yavaş ya da yüklü bir makinede aynı
sonucu verir; rapordaki süre yalnızca bir ölçümdür. Birkaç dışsal veya yalnızca
rapor amaçlı sınır çok yüksek bir güvenlik süresi tutar (fTetWild
çözücüsü, Graft'ın dış iptali) ya da uyarı üretir (Dressing'in yumuşak bütçesi);
bunlar belgelenmiş istisnalardır ve önemli olduklarında raporda belirtilir.
fTetWild katmanı (yalnızca stage 1'in kapatamadığı mesh'lerde kullanılır) bir
istisna daha ekler: iki çalıştırmada yüzeyi biraz farklı üçgenlere bölebilir,
ancak şekil ve baskıya uygunluk aynı kalır.

## Neden aşamalı hat

* **Aşama 1 - yerel motor (Rust `sutura_geom` + `sutura_engine`).** Yinelenen ve dejenere yüzleri kaldırır,
  non-manifold kenar ve köşeleri onarır, yüzleri tutarlı şekilde yönlendirir,
  *her* boyuttaki deliği kapatır ve küçük açık döküntü bileşenlerini atar. Filtreler,
  Sutura'nın üzerine kurulduğu kanıtlanmış VCG `meshing_*` semantiğini yansıtır
  ve yerel olarak yeniden uygulanmıştır (çalışma zamanında PyMeshLab yok), böylece
  hat bağımlılık açısından hafif ve izin verici lisanslı kalır.
* **Aşama 2 - Manifold (Rust çekirdeğinde).** Kapalı meshi geçerli bir
  manifold katı olarak yeniden kurar ve örtüşen kabukları boolean birleşimiyle
  birleştirir. Bu, Bambu Studio'nun da kullandığı C++ Manifold çekirdeğidir ve
  zorunlu `sutura_geom` eklentisinin içine derlenmiştir; çıktının tek, kapalı
  bir iki-manifold olmasını garanti eder.

Her aşama diğerinin yapamadığını düzeltir: Aşama 1 büyük delikleri kapatır ama
kendisiyle kesişen geometriyi çözmez; Manifold su geçirmez bir sonuç garanti
eder ama açık kenarlı herhangi bir girdiyi reddeder (`NotManifold`), bu yüzden
aşama 1'in meshi önce bitirmesi gerekir.

Aşama 2 her platformda zorunlu `sutura_geom` Rust eklentisinin içinde süreç
içi (in-process) çalışır. Aşama 2 için Python 3.11 sanal ortamı ve alt süreç
köprüsü yoktur: eklenti Manifold C++ çekirdeğini statik olarak bağlar. Başarısız bir yeniden
kurma raporda açıkça belirtilir, asla sessizce atlanmaz.

**İsteğe bağlı katmanlar.** İki çekirdek aşamanın etrafında, self-intersection
içeren ve ağır bozuk mesh'ler için katmanlar bulunur. Deneysel olanlar
varsayılan olarak kapalıdır ve kendi CLI bayrağı ya da GUI'deki **Seçenekler** penceresindeki bir kutuyla açılır;
fTetWild yedek katmanı ise varsayılan olarak kurulu ve
açıktır:

* *Stage 1'den önce:* `--experimental-autorefine` (kesişen üçgenleri kesişim
  doğruları boyunca böler, hiçbir yüzü silmez) ve
  `--experimental-indirect-autorefine` (aynı fikir, Rust `sutura-geom`
  çekirdeğinde exact predikatlarla; değerlendirme amaçlıdır). `auto`
  modunda model `mechanical` veya `dense_scan_heavy_si` şablonlarına uyuyorsa ve
  birbirine temas eden veya çakışan birden fazla kapalı bileşen içeriyorsa
  (örneğin Rubik küpü tarzı montajlar), **dış kabuk çıkarma** ön adımı hafif bir
  genişletmeyle (`5e-4 * bbox_diagonal`, 100 mm'lik nesne için ~0,05 mm) ve
  boolean birleşimle iç arayüzleri çözerek iç yüzeyleri ve kendini kesmeleri
  ana onarım geçişlerinden önce yok eder.
* *Stage 1'den sonra, son çare olarak:* fTetWild yedek katmanı (orijinal
  girdiyi fTetWild ile tetrahedralize edip su geçirmez bir yüzey çıkarır;
  varsayılan olarak kurulur, ~6 MB, bkz. Kurulum). Varsayılan olarak
  çalışır, ama yalnızca stage 1 hâlâ delik veya non-manifold kenar
  bırakıyorsa; `--no-fallback-ftetwild` (GUI: *Seçenekler* → *Onarım* sekmesi → *fTetWild fallback* işaretini
  kaldırın) kapatır, `--experimental-fallback-ftetwild` (GUI: *Seçenekler* → *Onarım* sekmesi → altındaki *+ self-intersection
  (yavaş)* kutusu) kapalı ama hâlâ self-intersection içeren
  sonuçlarda da çalıştırır. fTetWild'ın kendi tetrahedra kalite
  optimizasyonu kapalıdır (yalnızca dış yüzey kullanılır, çoğunlukla süre
  kaybıdır); `--ftetwild-optimize` (GUI: *Tetrahedra optimizasyonu*,
  önerilmez) açar. Alternatif olarak, deneysel kesin yeniden inşa **CDT**
  (`--solid-engine cdt`; GUI: *Seçenekler* → *Onarım* → *Katı motoru*) bu son
  çarenin yerini alır ve başarısız olursa ya da zaman aşımına uğrarsa
  fTetWild'a geçer.
* *Full Mend içinde, Stage 1'den sonra:* **guard'lı self-intersection kesme +
  yeniden kapatma** artakalan self-intersection yüzlerini (yüzler ve bir
  halkalık kuşak; bu tur mevcut sınırı azaltıp yine de kapatamazsa aynı tur bir
  kez iki halkalık kuşakla yeniden denenir ve retry bağlı bileşen sayısını
  artırırsa reddedilir) kaldırır, açılan döngüleri native delik doldurmasıyla
  (`selfintersection=True` — bu yeni kesişen kapakları *önler*) kapatır ve bir
  turu yalnızca self-intersection sayısı kesin azalırken, non-manifold kenarlar
  artmazken ve ağ tamamen kapanırken (sınır kenarı yok) kabul eder; aksi hâlde
  tur-öncesi ağ bayt bayt geri yüklenir. Self-intersection tespiti exact Rust
  sınıflandırıcısını kullanır. Hiçbir bağlı bileşen düşürülmez, en fazla üç tur
  çalışır (işi tur sayısı ve tur başına %30 yüz freni sınırlar; raporda
  gösterilen süre yalnızca bir ölçümdür) ve self-intersection içermeyen ağlarda
  no-op'tur. `SUTURA_SI_EXCISE=0` ile kapatılır.

Stage 1'den sonraki bu katmanlar **derin onarım merdivenini** oluşturur ve
`--deep-repair {off,local,full}` ile seçilir (ya da `SUTURA_DEEP_REPAIR` veya
`~/.config/sutura/config.json` içindeki `deep_repair` anahtarıyla; öncelik
bayrak, ortam değişkeni, config sırasıdır). CLI ve dosya yöneticisi menüsü
için varsayılan `full`'dur ve fTetWild katmanını yukarıda anlatıldığı gibi
çalıştırır. `off` hiçbir katmanı çalıştırmaz; delik veya non-manifold kenar
kalırsa JSON raporu `deep_repair.available` alanını taşır (kalan delik ve
non-manifold kenar sayısı, hâlâ çalıştırılabilecek katmanlar ve kaba bir süre
tahmini `estimate_s`; tahmin katsayıları corpus üzerinde kalibre edilene kadar
yer tutucudur) ve `--human` bir "Deep repair available" satırı gösterir.
`local`, yalnızca bozuk bölgeleri yeniden ören deneysel bir prototiptir:
sınır veya non-manifold kenar üzerindeki yüzler ve bir vertex halkalık
komşulukları silinir, açıklıklar yerel refine edilen delik kapatmasıyla
kapatılır ve yeni iç vertex'ler sınır sabit tutularak yumuşatılır; ancak delik
ve non-manifold kenar sayısı kötüleşmez, self-intersection artmaz ve silinen
bölge dışındaki her yüz değişmeden kalırsa benimsenir. Yalnızca küçük ve
basit hasarda çalışır (non-manifold kenar yok, sınır döngüleri en fazla 16
kenar, en fazla 8 hasarlı bölge ve 2.000 silinen yüz, 10 sn); delik
refinement'ı kapalıdır, çünkü 90 bin yüzlü bir mesh'te ~9 sn sürüp hiçbir
sonucu değiştirmedi. 40 gerçek örnekte 3 mesh'i daha kesin su geçirmez yaptı
(31 → 34); 115 mesh'lik corpus'ta hiçbirini yapmadı, bu yüzden `full`'un
parçası değildir.

Sıralı yedek yöntemler denenmeden önce otomatik yol, tespit edilen hasarın
anlamsız kıldığı bir yöntemi atlayabilir: kopya yüzler Join, iç yüzler veya bir
iç bölge Transplant ve Wall Thicken yöntemlerini atlar. Bu yalnızca yedek
sırasını değiştirir, temel onarımı asla; önceki sırayı geri getirmek için
`SUTURA_TRIAGE_PRUNE=0` ayarlayın.

**Self-intersection politikası** `--si-mode {repair,report,off}` ile seçilir
(ya da `SUTURA_SI_MODE`, ya da GUI Seçenekler → Onarım açılır listesi *Kendisiyle-kesişim*
[*Yalnızca bildir* / *Onar* / *Ölçme*]).
`report` (varsayılan) kalan self-intersection sayısını ölçüp bildirmeye devam
eder, ancak self-intersection'lar artık kapalı bir 2-manifold yüzeyde
`watertight` sınıflandırma kararını engellemez ve tek başına derin onarım
merdivenini tetiklemez — otomatik katmanları ve nihai kararı yalnızca delik
ve non-manifold kenarlar belirler; sayı `stage1.self_intersections_remaining`
alanında görünür kalır. Sıfır delik ve sıfır non-manifold kenara sahip bir mesh,
kalan self-intersection'lar olsa dahi `report` modunda `watertight` olarak
sınıflandırılır. `repair`, pozitif bir sayının hasar sayıldığı ve otomatik
Dressing yedeğini tetikleyebildiği önceki davranışı geri getirir. `off`, kesin
sınıflandırıcıyı atlar ve sayıyı `null` ("ölçülmedi") olarak bildirir. Açık zorlama bayrakları
(`--experimental-dressing`, `--experimental-fallback-ftetwild`) politikayı
geçersiz kılar ve kalan self-intersection'lara yine de müdahale eder. Korumalı
`si_excise_recap` geçişi her modda kendi bağımsız sayımını korur. Seçilen mod
raporda `si_mode` (ve `deep_repair.si_mode`) olarak yer alır.

Her katman ancak sonucu, delik ve non-manifold kenar açısından düz
aşamalı sonuçtan kötü değilse benimsenir; yani çekirdek aşamalı yol her zaman
güvenlik ağı olarak kalır. Benimsenen bir fTetWild sonucunun çıktıdan
girdiye tek yönlü Hausdorff mesafesi sınır kutusu köşegeninin %1'ini aşarsa
(geçici eşik) sonuç korunur ama işaretlenir: rapor `shape_changed: true` ve
`shape_changed` sorun kodunu ("Şekil fTetWild yedek katmanıyla değişti")
taşır, kategori düşürülmez. macOS ölçümünde 40 örnekten thingi10k_1038441
(%3,7), 1038439 (%1,7) ve 224108 (%1,1) bu durumdadır. Varsayılan
(kalite optimizasyonu olmadan çalışan) fTetWild sınırı Manifold
geçişinden sonra bile delik ve non-manifold korumasından geçemezse, mesh
180 sn'lik bütçenin kalanı içinde optimizasyon açık olarak bir kez daha
tetrahedralize edilir; rapor iki koşuyu da listeler (`attempts`) ve
benimseneni belirtir (`adopted_attempt`). 300.000 yüzün üzerindeki
girdilerde fTetWild katmanı atlanır (`reject_reason: too_large`): 40
örnekte tamamlanan en yavaş koşu 90.000 yüzde ~55 sn sürdü, yani daha büyük
taramalar bütçe içinde bitemez; 115 mesh'lik corpus'taki yoğun Artec
taramaları da her koşuda zaman aşımına uğradı. fTetWild
eki kurulu değilse ve hiçbir bayrak verilmezse davranış tam olarak
yukarıdaki Aşama-1 + Aşama-2 çekirdeğidir.

40 gerçek örnek üzerinde ölçüldü (`scripts/benchmark_repair_corpus.py`,
2 vCPU'lu x86_64 VM): yedek katman olmadan kesin su geçirmez 31/40,
varsayılan yedek katmanla **39/40** (katman 9 mesh'te çalıştı, 8'inde
benimsendi; açık kalan `artec_metal-nut` mesh başına 180 sn'lik bütçeye
takıldı). Toplam süre 64 sn → 669 sn; ek süre yalnızca aksi halde açık
kalacak mesh'lerde harcanır. Kapalı ama self-intersection içeren sonuçlarda
da çalıştırmak (`--experimental-fallback-ftetwild`) aynı 39/40'ı 2.595 sn'de
verir ve zaten kapalı 7 mesh'i yeniden örgüler; bu yüzden isteğe bağlı
kalır. Uyarılar: fTetWild'ın çıktısı ve süresi çalıştırmadan çalıştırmaya
tekrarlanabilir değildir (ör. thingi10k_248395 tek iş parçacığıyla bile
30–160 sn sürdü, arada bütçeyi aştı); sonuç yeniden üçgenlenir (yüz sayıları
değişir, çoğunlukla daha kaba); girdinin her noktası çıktıya yakın kalır
(kontrol edilen mesh'lerde sınır kutusu köşegeninin %0,2–1,4'ü), çıktı ise
açıklıkların veya boşlukların kapatıldığı yerlerde girdiden uzakta yüzey
ekleyebilir (çıktıdan girdiye tek yönlü Hausdorff mesafesi thingi10k_224108,
1038439 ve 1038441'de %1–4, iki corpus mesh'inde %5–8; thingi10k_46012 için
daha önce verilen %28, iç tetrahedron vertex'lerini de örnekleyen bir
ölçümden geliyordu). fTetWild artık
mesh kalitesi optimizasyonu olmadan çalışır (`optimize=False`; optimizasyon
yalnızca atılan iç tetrahedronları iyileştirir): macOS'taki parametre
taramasında thingi10k_46012'yi 34 sn'den 6 sn'ye indirdi, Hausdorff mesafesi
%0,09 oldu; artec_metal-nut ise bütçeyi yine aştı. Yukarıdaki süreler ve
sayılar optimizasyon açıkken ölçülmüştür.

v0.9.1'den beri delik/non-manifold kontrolünü geçemeyen sınır sonrası yeniden
deneme belirleyicidir (kalan duvar saatine bağlı değildir) ve zaman bütçesi
yalnızca, önceki aşama sonucunu koruyan dış bir tümü-ya-da-hiç kesintisidir.
Çıktı yine de çalıştırmadan çalıştırmaya bayt-birebir değildir.
