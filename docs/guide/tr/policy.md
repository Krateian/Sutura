# Politika dosyası

[← README'ye dön](../../../README.tr.md) · [English](../policy.md)

Sutura, onarım karar kurallarını paketle birlikte gelen tek ve sürümlü bir
politika dosyasından okur. Kullanıcı dosyası yalnızca ayarlanabilir değerleri
geçersiz kılabilir; güvenlik kapıları değiştirilemez.

## Dosyaların yeri

- Yerleşik politika: `sutura/sutura_engine/policy/default_policy.toml`, paketin
  içinde gelir (düzenlenecek bir yol yoktur).
- Kullanıcı geçersiz kılması: `~/.config/sutura/policy.toml` (yoksa oluşturun).

Kullanıcı dosyası başlangıçta bir kez okunur ve yerleşik politikanın üzerine
birleştirilir. Değişiklik bir sonraki çalıştırmada geçerli olur.

## Neler değiştirilebilir

Yalnızca veri niteliğindeki değerler ayarlanabilir:

- `[intensity]` — yerleşik yoğunluk ön ayarları ve varsayılan ön ayar.
- `[modes]`, `[stage1_profiles]`, `[stage1_types]`, `[stage1_tuning]` — Aşama-1
  sabit modları, adlandırılmış Aşama-1 profilleri, ağ tipi eşikleri ve
  sınıflandırıcı güven ayar kapıları.
- `[ranking]`, `[templates]` — sıralama ayarları (tercih artışı, üst-N, öğrenme
  önceliği) ve puan şablonu eşikleri ile tercih edilen yöntem sıraları.
- `[tiers.*]` — katman bütçeleri ve ayarları (flap, tamponade, repeat, splint,
  coaptation, dressing) ve katmanların varsayılan etkinlik anahtarları.
- `[auto]`, `[order]` — otomatik yükseltme politikası ve katman sırası.

## Neler değiştirilemez

`[gates.*]` altındaki her şey, her `[tiers.<katman>.gates]` tablosu, tüm
`[si_escalation]` bölümü (otomatik SI adayı ve asla-daha-kötü hacim/mesafe
toleransları) ve `auto.generative_max_hausdorff_rel` **kilitli güvenlik
kapısıdır**: şekil ve asla-daha-kötü sınırları (benimseme koruması, Tamponade
şekil ve sızıntı tetikleyicileri, Flap moloz eşikleri, Dressing benimseme
sınırları, Hausdorff örnek tabanı ve decimation / fTetWild marjları). Kilitli
bir anahtarın geçersiz kılınması uyarıyla yok sayılır; etkin politika her zaman
yerleşik kapı değerlerini korur.

Bilinmeyen anahtarlar ve bozuk dosyalar uyarıyla yok sayılır (hiç
ayrıştırılamayan dosyada yerleşik politikaya dönülür ve rapor `policy_error`
kaydeder). Raporun `policy` bloğu şema sürümünü, etkin politikanın içerik
özetini, kullanıcı geçersiz kılmasının uygulanıp uygulanmadığını ve yok sayılan
anahtarları taşır — dosya yolları taşınmaz.

## Sıfırlama

Yerleşik politikaya dönmek için `~/.config/sutura/policy.toml` dosyasını silin.
