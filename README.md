<div align="center">
  <img src="assets/ghlogo.png" alt="Inari Syndicate" width="260">

  # Inari

  **Inari Syndicate için yüksek performanslı Windows multiboxing aracı.**

  [Nicotine](https://github.com/isomerc/nicotine)'den çatallandı, [hikanteki.com](https://hikanteki.com/) kimliğine uyarlandı.
</div>

---

## Özellikler

- **Anında istemci geçişi** — fare yan tuşları veya klavye kısayollarıyla (F10/F11 varsayılan)
- **DWM önizleme pencereleri** — her EVE istemcisi için canlı küçük resim; tıklayınca o istemciye geçer
- **Liste görünümü** — karakter adlarını gösteren, her zaman üstte duran kompakt pencere; aktif karakter işaretli
- **Karaktere özel kısayollar** — belirli bir istemciye doğrudan atlama
- **Scout desteği** — döngüde değil, ama kısayolla erişilebilir karakterler
- **Çoklu değiştirici** — `Ctrl+Shift+Num 1` gibi kombolar, numpad tuşları dâhil
- **Otomatik üst üste dizme** — birden çok EVE istemcisini kusursuz biçimde ortalar
- **Ekran çözünürlüğünü otomatik algılar** — her monitör kurulumunda çalışır
- **Aktif olmayan istemcileri küçült** — kaynak tüketimini azaltmak için isteğe bağlı
- **Canlı yapılandırma** — paneldeki değişiklikler anında uygulanır

## Hızlı Kurulum

[GitHub Releases sayfasından](https://github.com/Hikan-Teki/nicotine_plus/releases) en son `Inari.exe` dosyasını indirip çift tıklayın. İlk çalıştırmada yapılandırma paneli açılır ve `%APPDATA%\inari\config.toml` altında varsayılan bir yapılandırma oluşturulur.

## Kullanım

`Inari.exe`'ye çift tıklamak `inari start` ile aynıdır — daemon'u başlatır ve yapılandırma panelini açar.

### Temel Komutlar

```
inari start          # Her şeyi başlat (daemon + önizlemeler)
inari stop           # Tüm Inari süreçlerini durdur
inari stack          # Tüm EVE pencerelerini üst üste diz
inari forward        # Bir sonraki istemciye geç
inari backward       # Bir önceki istemciye geç
inari 1              # 1 numaralı istemciye atla
inari 2              # 2 numaralı istemciye atla
```

### Hedefli Geçiş

Varsayılan olarak `inari 1`, `inari 2` vb. pencere algılama sırasını kullanır. Kendi sıranızı tanımlamak için `config.toml` içinde `characters` listesine karakter adlarını yazın:

```toml
characters = [
  "Ana Karakter",
  "Alt 1",
  "Alt 2",
]
```

Sıra 1 = hedef 1, sıra 2 = hedef 2 vb.

### Scout Karakterleri (Döngü Dışı)

13 karakter çalıştırıyorsanız ama yalnızca 12'sinin döngüye girmesini istiyorsanız (13. karakter scout olarak sadece kısayolla erişilsin), yapılandırma panelinde o satırdaki "döngüde" kutucuğunu kapatın. Karakter listede ve önizlemede görünür kalır, bağlı kısayolu çalışmaya devam eder, ama ileri/geri geçişte atlanır. `switch N` komutu bu karakterlere de ulaşır.

### Kısayollar

Panelden veya `config.toml` dosyasından düzenleyin:

```toml
enable_keyboard_buttons = true
forward_key  = 0x7A  # VK_F11
backward_key = 0x79  # VK_F10
modifier_key = 0     # Geri için basılı tutulacak isteğe bağlı tuş
```

Karakter başına kısayollar için panelde her satırdaki `Ctrl` / `Shift` / `Alt` kutularını işaretleyin, sonra bağlama düğmesine basıp istediğiniz tuşa (numpad dâhil) basın. Böylece `Ctrl+Num 1`, `Ctrl+Shift+F11` gibi kombolar çalışır.

Fare yan tuşları varsayılan olarak kapalıdır (tarayıcıdaki ileri-geri tuşlarıyla çakışmaması için). Panelden açabilirsiniz.

## Yapılandırma

Yapılandırma dosyası: `%APPDATA%\inari\config.toml`

İlk çalıştırmada otomatik oluşturulur. Temel ayarlar:

```toml
display_width = 1920
display_height = 1080
panel_height = 0            # Taskbar/panel varsa buraya yazın
eve_width = 1037            # Ekran genişliğinin ~%54'ü
eve_height = 1080
enable_mouse_buttons = false
forward_button = 2          # XBUTTON2 (ileri yan tuş)
backward_button = 1         # XBUTTON1 (geri yan tuş)
enable_keyboard_buttons = true
forward_key = 0x7A          # VK_F11
backward_key = 0x79         # VK_F10
minimize_inactive = false   # Geçişte aktif olmayanı küçült
preview_width = 320
preview_height = 180
show_previews = true        # false yaparsanız sadece kısayollarla çalışır
positions_locked = false
```

## Mimari

- **Daemon modu**: Pencere durumunu bellekte tutar, geçişler anında olur
- **Adlandırılmış pipe IPC**: ~2 ms komut gecikmesi (süreç başlatmaya göre ~50-100 ms kazanç)
- **Yerel giriş kancaları**: Düşük seviye klavye + fare kancaları
- **DWM küçük resimleri**: Windows Desktop Window Manager API'siyle canlı önizleme pencereleri

## Kaynak Koddan Derleme

```
# Rust kurun (https://rustup.rs)
cargo build --release

# Çıktı: target\release\Inari.exe
```

## Teşekkürler

Bu proje [isomerc/nicotine](https://github.com/isomerc/nicotine)'in Windows portundan forklanmıştır. Orijinal çalışmanın kendisi [EVE-O Preview](https://github.com/EveOPreview/EveOPreview)'dan ilham almıştır. Inari Syndicate markasına göre yeniden tasarlanmış ve Türkçeleştirilmiştir.

## Lisans

[LICENSE](LICENSE.md) dosyasına bakınız.
