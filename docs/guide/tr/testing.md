# Test ve benchmark (geliştiriciler)

[← README'ye dön](../../../README.tr.md) · [English](../testing.md)

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
# İkisi de depo içindeki sutura/repair.py'yi çalıştırır; CLI'nin kendi Python'u ile çalıştırın:
~/.local/share/sutura/venv/bin/python tests/make_layered_multiobject_3mf.py --check   # katmanlı çok nesneli 3MF
~/.local/share/sutura/venv/bin/python tests/test_adversarial.py                       # bozuk girdi işleme
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

Benchmark harness'i (`scripts/benchmark_repair_corpus.py`) ayrıca bir
**Manifold çapraz-doğrulama sütunu** taşır: onarılan her mesh'in nihai
geometrisi, dizi tabanlı `defects.detect()` sıkı kontrolünün yanında
`sutura_geom.watertight_check` ile bağımsız olarak kontrol edilir (bir Manifold
`NoError` ile kurulur ve boş değilse ⇒ su geçirmez) ve iki sonuç
karşılaştırılır. Sıkı sonucu ya da hattı asla değiştirmez; Manifold çekirdeği
`sutura_geom` içinde zorunlu olduğu için sütun her kurulumda doludur.
`--experimental-indirect-autorefine` her mesh için exact Rust arrangement
katmanını çalıştırır; geliştirici kancası `--cdt-experimental BITS` ise
değerlendirilmiş ama varsayılan olmayan CDT varyantlarını açar (bkz. crate
README'si, "Sloan flips and edge-point propagation").

İşkence testleri zor ama basılabilir geometriyi kapsar:

```sh
~/.local/share/sutura/venv/bin/python tests/torture_tests.py
```

Rust çekirdeği (`rust/sutura-geom`, Rust araç zinciri gerekir; sonuçlar crate
README'sinde):

```sh
cd rust/sutura-geom
cargo test --release                      # birim + filtre-exact fark testleri
cargo run --release --example bench_arrangement --features profile -- mesh.obj
```

Uzantının Python smoke testleri (`tests/test_sutura_geom*.py`), uzantının
etkin ortama `maturin develop` ile kurulmuş olmasını gerektirir. Elle derleme
yapmadan otomatik kurulum için `scripts/install_sutura_geom.sh`, yayınlanan
eşleşen doğrulanmış hazır tekerlekleri indirir.

### Geliştirici aracı — sentetik defekt enjeksiyonu

`scripts/defect_injector.py` bir geliştirici aracıdır: bir mesh'i bilerek
bozar — seçilen defekt tiplerini (`--hole`, `--non-manifold`, `--self-intersect`,
`--flipped-normal`, `--degenerate`, herhangi bir kombinasyon; `--count N`,
`--seed N`) enjekte eder ve bozuk kopyayı yeni bir dosyaya yazar — girdi asla
değiştirilmez. Onarım hattı ve defekt algılayıcıları için sentetik bozuk
mesh'ler üretmekte ve gelecekteki sentetik eğitim/test setlerini genişletmekte
kullanılabilir. Her enjekte edilen tip, `tests/test_defect_injector.py`
tarafından ilgili algılayıcıya karşı doğrulanır (`defects.detect()` delikler/
non-manifold için, `defect_type_colors()` flipped/dejenere için, yerel
self-intersection sınıflandırıcısı self-intersect için).

Bu beş senaryoyu çalıştırır ve her biri için önce/sonra raporlar: bir 5M
üçgenli küre (onarım süresi), 0.05 mm ince levha (özellik kaybı riski — sağlam
kalmalıdır), çok parçalı montaj (8 yüzlü döküntü kaldırma eşiği meşru
parçaları silmemelidir), çok sayıda mikro çatlak içeren kaba bir tarama-tarzı
mesh (kalan delik beklentisi) ve `--mode extreme` ile çalıştırılan iç içe
geçmiş iki küre — ek self-intersection geçişleri kesişen her yüzü kaldırmalı
ve `extreme_passes_applied=True` bildirmelidir.
