import requests
from django.core.mail import send_mail
from django.db import transaction

from .tasks import ship_order


@transaction.atomic
def place_order(cart, user):
    order = cart.to_order()
    requests.post("https://pay.example.lk/charge", json={"amount": order.total})
    send_mail("Order placed", f"Order {order.id}", "shop@example.lk", [user.email])
    ship_order.delay(order.id)
    return order


def refresh_rates():
    with transaction.atomic():
        rates = requests.get("https://rates.example.lk/today").json()
        save_rates(rates)
